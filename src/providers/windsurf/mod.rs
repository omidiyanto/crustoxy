//! Windsurf provider — native gRPC integration with the Windsurf language server.
//!
//! Auto-enabled when CODEIUM_AUTH_TOKEN is set. Supports both:
//!   - Cascade flow (modern models with modelUid)
//!   - RawGetChatMessage (legacy models with enumValue only)

pub mod accounts;
pub mod builders;
pub mod grpc;
pub mod ls;
pub mod models;
pub mod parsers;
pub mod proto;

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::models::anthropic::{MessagesRequest, extract_text_from_system};

use self::grpc::{GrpcSession, default_csrf_token};
use self::ls::LanguageServer;
use self::parsers::{
    STEP_STATUS_DONE, STEP_STATUS_GENERATING, STEP_TYPE_ERROR_MESSAGE, STEP_TYPE_PLANNER_RESPONSE,
    TrajectoryStatus,
};

/// gRPC service paths for the language server.
const SVC: &str = "/exa.language_server_pb.LanguageServerService";

/// Bundles the per-request context passed to stream_cascade / stream_raw.
struct StreamCtx<'a> {
    messages: &'a [(String, String)],
    system_prompt: &'a str,
    model: &'a models::WindsurfModel,
    request_id: &'a str,
    original_model: &'a str,
    input_tokens: u32,
    tx: &'a tokio::sync::mpsc::Sender<bytes::Bytes>,
}

/// Exchange a Codeium auth token for a Windsurf API key.
/// Tries register.windsurf.com first, falls back to api.codeium.com.
pub async fn register_codeium_token(token: &str) -> Result<(String, String), String> {
    // Strip ott$ prefix if present (WindsurfAPI compatibility)
    let clean_token = token.strip_prefix("ott$").unwrap_or(token);

    let body = serde_json::json!({ "firebase_id_token": clean_token });
    let body_str = body.to_string();

    let new_url =
        "https://register.windsurf.com/exa.seat_management_pb.SeatManagementService/RegisterUser";
    let legacy_url = "https://api.codeium.com/register_user/";

    let client = reqwest::Client::new();
    let ua = "windsurf/1.9600.41";

    for (url, source) in [(new_url, "new"), (legacy_url, "legacy")] {
        let res = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("Accept", "application/json")
            .header("User-Agent", ua)
            .header("Origin", "https://windsurf.com")
            .header("Referer", "https://windsurf.com/")
            .body(body_str.clone())
            .send()
            .await;

        match res {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r
                    .json()
                    .await
                    .map_err(|e| format!("JSON parse error: {e}"))?;
                let api_key = data
                    .get("api_key")
                    .or_else(|| data.get("apiKey"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("{source}: missing api_key in response"))?
                    .to_string();
                let api_server_url = data
                    .get("api_server_url")
                    .or_else(|| data.get("apiServerUrl"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if source == "legacy" {
                    warn!("RegisterUser fell back to legacy api.codeium.com");
                } else {
                    info!(
                        "RegisterUser via register.windsurf.com OK (key={}...)",
                        &api_key[..12.min(api_key.len())]
                    );
                }
                return Ok((api_key, api_server_url));
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                warn!(
                    "RegisterUser {source} HTTP {status}: {}",
                    &body[..200.min(body.len())]
                );
            }
            Err(e) => {
                warn!("RegisterUser {source} failed: {}", e);
            }
        }
    }

    Err("RegisterUser failed on both register.windsurf.com and api.codeium.com".to_string())
}

/// The Windsurf provider, managing a language server and handling chat requests.
pub struct WindsurfProvider {
    api_key: String,
    ls: Arc<Mutex<LanguageServer>>,
    /// Cloneable gRPC session handle — concurrent calls share the H2 connection
    grpc: Arc<GrpcSession>,
    csrf_token: String,
    session_id: String,
    workspace_initialized: Arc<Mutex<bool>>,
}

impl WindsurfProvider {
    /// Initialize: resolve API key (from accounts.json or token exchange),
    /// spawn LS, init session.
    pub async fn new(
        auth_token: &str,
        ls_path: &str,
        ls_port: u16,
        api_server_url: &str,
        data_dir: &str,
    ) -> Result<Self, String> {
        // ── Step 1: Resolve API key (load saved or exchange token) ──
        let mut accts = accounts::load_accounts(data_dir);
        let (resolved_api_key, resolved_server_url) =
            if let Some(existing) = accounts::find_account_by_token(&accts, auth_token) {
                info!(
                    "Reusing saved API key for account {} (key={}...)",
                    existing.id,
                    &existing.api_key[..12.min(existing.api_key.len())]
                );
                (existing.api_key.clone(), existing.api_server_url.clone())
            } else {
                info!("CODEIUM_AUTH_TOKEN set, exchanging for Windsurf API key...");
                let (key, server_url) = register_codeium_token(auth_token).await?;
                let effective_url = if server_url.is_empty() {
                    api_server_url.to_string()
                } else {
                    server_url
                };
                // Save to accounts.json
                let account = accounts::create_account(&key, &effective_url, auth_token);
                info!(
                    "Registered new account {} (key={}...)",
                    account.id,
                    &key[..12.min(key.len())]
                );
                accts.push(account);
                accounts::save_accounts(data_dir, &accts);
                (key, effective_url)
            };

        let effective_server_url = if resolved_server_url.is_empty() {
            api_server_url.to_string()
        } else {
            resolved_server_url
        };

        // ── Step 2: Spawn language server ──
        let csrf = default_csrf_token().to_string();
        let ls = LanguageServer::start(ls_path, ls_port, &csrf, &effective_server_url).await?;
        let grpc = Arc::new(GrpcSession::new(ls_port));
        let session_id = Uuid::new_v4().to_string();

        let provider = Self {
            api_key: resolved_api_key.clone(),
            ls: Arc::new(Mutex::new(ls)),
            grpc,
            csrf_token: csrf,
            session_id,
            workspace_initialized: Arc::new(Mutex::new(false)),
        };

        // ── Step 3: Initialize workspace ──
        provider.ensure_workspace_init().await?;

        info!(
            "Windsurf provider initialized (api_key={}...)",
            &resolved_api_key[..8.min(resolved_api_key.len())]
        );
        Ok(provider)
    }

    pub async fn is_healthy(&self) -> bool {
        let mut ls = self.ls.lock().await;
        ls.is_alive() && ls.is_ready()
    }

    pub async fn try_restart(&self) -> Result<(), String> {
        let mut ls = self.ls.lock().await;
        ls.restart().await?;
        let _port = ls.port;
        drop(ls);
        // Recreate gRPC session for new port
        self.grpc.close().await;
        // Re-initialize workspace
        let mut initialized = self.workspace_initialized.lock().await;
        *initialized = false;
        drop(initialized);
        self.ensure_workspace_init().await
    }

    pub async fn shutdown(&self) {
        let mut ls = self.ls.lock().await;
        ls.stop().await;
        self.grpc.close().await;
    }

    /// One-shot workspace initialization for Cascade flow.
    async fn ensure_workspace_init(&self) -> Result<(), String> {
        let mut initialized = self.workspace_initialized.lock().await;
        if *initialized {
            return Ok(());
        }

        // 1. InitializeCascadePanelState
        let req = builders::build_initialize_panel_state_request(&self.api_key, &self.session_id);
        self.grpc
            .unary(
                &format!("{}/InitializeCascadePanelState", SVC),
                &req,
                &self.csrf_token,
                10000,
            )
            .await
            .map_err(|e| format!("InitializeCascadePanelState failed: {}", e))?;
        debug!("Windsurf: InitializeCascadePanelState OK");

        // 2. AddTrackedWorkspace
        let workspace = "/tmp/windsurf-workspace";
        std::fs::create_dir_all(workspace).ok();
        let req = builders::build_add_tracked_workspace_request(workspace);
        self.grpc
            .unary(
                &format!("{}/AddTrackedWorkspace", SVC),
                &req,
                &self.csrf_token,
                10000,
            )
            .await
            .map_err(|e| format!("AddTrackedWorkspace failed: {}", e))?;
        debug!("Windsurf: AddTrackedWorkspace OK");

        // 3. UpdateWorkspaceTrust
        let req = builders::build_update_workspace_trust_request(&self.api_key, &self.session_id);
        self.grpc
            .unary(
                &format!("{}/UpdateWorkspaceTrust", SVC),
                &req,
                &self.csrf_token,
                10000,
            )
            .await
            .map_err(|e| format!("UpdateWorkspaceTrust failed: {}", e))?;
        debug!("Windsurf: UpdateWorkspaceTrust OK");

        // 4. Heartbeat
        let req = builders::build_heartbeat_request(&self.api_key, &self.session_id);
        self.grpc
            .unary(&format!("{}/Heartbeat", SVC), &req, &self.csrf_token, 10000)
            .await
            .map_err(|e| format!("Heartbeat failed: {}", e))?;
        debug!("Windsurf: Heartbeat OK");

        *initialized = true;
        info!("Windsurf: workspace initialized");
        Ok(())
    }

    /// Convert Anthropic MessagesRequest into (role, content) pairs + system prompt.
    fn prepare_messages(request: &MessagesRequest) -> (Vec<(String, String)>, String) {
        let system_text = extract_text_from_system(&request.system);
        let mut messages = Vec::new();
        for msg in &request.messages {
            let role = msg.role.clone();
            let content = crate::models::anthropic::extract_text_from_content(&msg.content);
            if role != "system" {
                messages.push((role, content));
            }
        }
        (messages, system_text)
    }

    /// Stream a chat response via the Windsurf language server.
    pub fn stream_response(
        self: &Arc<Self>,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> tokio_stream::wrappers::ReceiverStream<bytes::Bytes> {
        let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
        let provider = Arc::clone(self);
        let request = request.clone();
        let request_id = request_id.to_string();

        tokio::spawn(async move {
            let result = provider
                .do_stream(&request, input_tokens, &request_id, &tx)
                .await;
            if let Err(e) = result {
                error!("Windsurf stream error: {}", e);
                if e.contains("connect") || e.contains("H2") || e.contains("handshake") {
                    warn!("Windsurf: attempting auto-restart after connection error");
                    if let Err(re) = provider.try_restart().await {
                        error!("Windsurf: auto-restart failed: {}", re);
                    }
                }
                let error_event = crate::sse::format_error_event(&e, "api_error");
                let _ = tx.send(bytes::Bytes::from(error_event)).await;
            }
        });

        tokio_stream::wrappers::ReceiverStream::new(rx)
    }

    async fn do_stream(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
        tx: &tokio::sync::mpsc::Sender<bytes::Bytes>,
    ) -> Result<(), String> {
        let (messages, system_prompt) = Self::prepare_messages(request);
        let model_name = &request.model;
        let ws_model = models::resolve_model(model_name)
            .ok_or_else(|| format!("Unknown Windsurf model: {}", model_name))?;

        info!(
            "Windsurf [{}]: model={} uid={} cascade={}",
            request_id,
            ws_model.name,
            ws_model.model_uid,
            ws_model.use_cascade()
        );

        let original_model = request.original_model.as_deref().unwrap_or(&request.model);
        let start_event =
            crate::sse::format_message_start(request_id, original_model, input_tokens);
        tx.send(bytes::Bytes::from(start_event))
            .await
            .map_err(|_| "Channel closed".to_string())?;

        let ctx = StreamCtx {
            messages: &messages,
            system_prompt: &system_prompt,
            model: ws_model,
            request_id,
            original_model,
            input_tokens,
            tx,
        };

        if ws_model.use_cascade() {
            self.stream_cascade(&ctx).await?;
        } else {
            self.stream_raw(&ctx).await?;
        }

        Ok(())
    }

    /// Stream via Cascade flow — NO long-held mutex lock.
    /// Each gRPC call acquires/releases independently via the shared GrpcSession.
    async fn stream_cascade(&self, ctx: &StreamCtx<'_>) -> Result<(), String> {
        let StreamCtx {
            messages,
            system_prompt,
            model,
            request_id,
            original_model,
            input_tokens,
            tx,
        } = ctx;
        // 1. StartCascade → cascade_id
        let req = builders::build_start_cascade_request(&self.api_key, &self.session_id);
        let resp = self
            .grpc
            .unary(
                &format!("{}/StartCascade", SVC),
                &req,
                &self.csrf_token,
                30000,
            )
            .await?;
        let cascade_id = parsers::parse_start_cascade_response(&resp);
        if cascade_id.is_empty() {
            return Err("StartCascade returned empty cascade_id".to_string());
        }
        debug!("Windsurf [{}]: cascade_id={}", request_id, cascade_id);

        // 2. Build the user message text
        let mut text_parts = Vec::new();
        if !system_prompt.is_empty() {
            text_parts.push(system_prompt.to_string());
            text_parts.push(String::new());
        }
        for (role, content) in *messages {
            let tag = if role == "user" || role == "tool" {
                "human"
            } else {
                "assistant"
            };
            text_parts.push(format!("<{}>\n{}\n</{}>", tag, content, tag));
        }
        let user_text = text_parts.join("\n\n");

        // 3. SendUserCascadeMessage
        let req = builders::build_send_cascade_message_request(
            &self.api_key,
            &cascade_id,
            &user_text,
            model.enum_value,
            model.model_uid,
            &self.session_id,
        );
        self.grpc
            .unary(
                &format!("{}/SendUserCascadeMessage", SVC),
                &req,
                &self.csrf_token,
                30000,
            )
            .await?;
        debug!("Windsurf [{}]: SendUserCascadeMessage OK", request_id);

        // 4. Poll GetCascadeTrajectorySteps — per-step cursor tracking like WindsurfAPI
        let mut step_offset: u64 = 0;
        let mut yielded_by_step: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        let mut total_yielded: usize = 0;
        let mut saw_text = false;
        let mut saw_active = false;
        let mut idle_count = 0;
        let poll_interval = std::time::Duration::from_millis(500);
        let max_wait = std::time::Duration::from_secs(300);
        let started = std::time::Instant::now();
        let mut last_growth_at = std::time::Instant::now();
        let idle_grace = std::time::Duration::from_secs(5);
        let no_growth_stall = std::time::Duration::from_secs(25);

        loop {
            if started.elapsed() > max_wait {
                warn!(
                    "Windsurf [{}]: max_wait timeout ({}s)",
                    request_id,
                    max_wait.as_secs()
                );
                break;
            }

            tokio::time::sleep(poll_interval).await;

            // Get new steps
            let steps_req = builders::build_get_trajectory_steps_request(&cascade_id, step_offset);
            let steps_resp = self
                .grpc
                .unary(
                    &format!("{}/GetCascadeTrajectorySteps", SVC),
                    &steps_req,
                    &self.csrf_token,
                    10000,
                )
                .await?;
            let steps = parsers::parse_trajectory_steps(&steps_resp);

            for (i, step) in steps.iter().enumerate() {
                let abs_idx = step_offset as usize + i;

                // Check for errors
                if !step.error_text.is_empty() {
                    return Err(format!("Cascade error: {}", step.error_text));
                }
                if step.step_type == STEP_TYPE_ERROR_MESSAGE {
                    let msg = if step.error_text.is_empty() {
                        "Unknown cascade error"
                    } else {
                        &step.error_text
                    };
                    return Err(format!("Cascade error (step type 17): {}", msg));
                }

                // Only process planner response steps
                if step.step_type != STEP_TYPE_PLANNER_RESPONSE {
                    continue;
                }
                if step.status != STEP_STATUS_GENERATING && step.status != STEP_STATUS_DONE {
                    continue;
                }

                // Use response_text for streaming (monotonic, append-only)
                let live_text = if !step.response_text.is_empty() {
                    &step.response_text
                } else {
                    &step.text
                };
                if live_text.is_empty() {
                    continue;
                }

                let prev = yielded_by_step.get(&abs_idx).copied().unwrap_or(0);
                if live_text.len() > prev {
                    let delta = &live_text[prev..];
                    yielded_by_step.insert(abs_idx, live_text.len());
                    total_yielded += delta.len();
                    last_growth_at = std::time::Instant::now();
                    saw_text = true;

                    let delta_event = crate::sse::format_content_delta(delta, 0);
                    tx.send(bytes::Bytes::from(delta_event))
                        .await
                        .map_err(|_| "Channel closed".to_string())?;
                }
            }

            step_offset += steps.len() as u64;

            // Warm stall detection
            if saw_text && last_growth_at.elapsed() > no_growth_stall {
                warn!(
                    "Windsurf [{}]: warm stall after {}s",
                    request_id,
                    last_growth_at.elapsed().as_secs()
                );
                break;
            }

            // Check trajectory status
            let status_req = builders::build_get_trajectory_request(&cascade_id);
            let status_resp = self
                .grpc
                .unary(
                    &format!("{}/GetCascadeTrajectory", SVC),
                    &status_req,
                    &self.csrf_token,
                    10000,
                )
                .await?;
            let status = parsers::parse_trajectory_status(&status_resp);

            if status != TrajectoryStatus::Idle {
                saw_active = true;
            }

            if status == TrajectoryStatus::Idle {
                if !saw_active && started.elapsed() < idle_grace {
                    continue;
                }
                idle_count += 1;
                let growth_settled = last_growth_at.elapsed() > poll_interval * 2;
                let can_break = if saw_text {
                    idle_count >= 2 && growth_settled
                } else {
                    idle_count >= 4
                };
                if can_break {
                    break;
                }
            } else {
                idle_count = 0;
            }
        }

        // Send message_stop event
        let output_tokens = (total_yielded / 4) as u32;
        let stop_event = crate::sse::format_message_stop(
            request_id,
            original_model,
            *input_tokens,
            output_tokens,
            "end_turn",
        );
        tx.send(bytes::Bytes::from(stop_event))
            .await
            .map_err(|_| "Channel closed".to_string())?;

        info!(
            "Windsurf [{}]: cascade complete, ~{} chars",
            request_id, total_yielded
        );
        Ok(())
    }

    /// Stream via RawGetChatMessage (legacy, for enum-only models).
    async fn stream_raw(&self, ctx: &StreamCtx<'_>) -> Result<(), String> {
        let StreamCtx {
            messages,
            system_prompt,
            model,
            request_id,
            original_model,
            input_tokens,
            tx,
        } = ctx;
        let req = builders::build_raw_get_chat_message_request(
            &self.api_key,
            messages,
            system_prompt,
            model.enum_value,
            model.name,
            &self.session_id,
        );

        let mut rx = self
            .grpc
            .stream(
                &format!("{}/RawGetChatMessage", SVC),
                &req,
                &self.csrf_token,
                300000,
            )
            .await?;

        let mut total_text = String::new();
        let mut last_text_len = 0;

        while let Some(result) = rx.recv().await {
            match result {
                Ok(frame_data) => {
                    let parsed = parsers::parse_raw_response(&frame_data);
                    if parsed.is_error {
                        return Err(format!("RawGetChatMessage error: {}", parsed.text));
                    }
                    if !parsed.text.is_empty() && parsed.text.len() > last_text_len {
                        let new_text = &parsed.text[last_text_len..];
                        if !new_text.is_empty() {
                            let delta_event = crate::sse::format_content_delta(new_text, 0);
                            tx.send(bytes::Bytes::from(delta_event))
                                .await
                                .map_err(|_| "Channel closed".to_string())?;
                        }
                        last_text_len = parsed.text.len();
                        total_text = parsed.text;
                    }
                    if !parsed.in_progress {
                        break;
                    }
                }
                Err(e) => return Err(format!("RawGetChatMessage stream error: {}", e)),
            }
        }

        let output_tokens = (total_text.len() / 4) as u32;
        let stop_event = crate::sse::format_message_stop(
            request_id,
            original_model,
            *input_tokens,
            output_tokens,
            "end_turn",
        );
        tx.send(bytes::Bytes::from(stop_event))
            .await
            .map_err(|_| "Channel closed".to_string())?;

        info!(
            "Windsurf [{}]: raw complete, ~{} chars",
            request_id,
            total_text.len()
        );
        Ok(())
    }

    /// Non-streaming response.
    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> Result<serde_json::Value, String> {
        let (messages, system_prompt) = Self::prepare_messages(request);
        let ws_model = models::resolve_model(&request.model)
            .ok_or_else(|| format!("Unknown Windsurf model: {}", request.model))?;
        let original_model = request.original_model.as_deref().unwrap_or(&request.model);

        let req = builders::build_raw_get_chat_message_request(
            &self.api_key,
            &messages,
            &system_prompt,
            ws_model.enum_value,
            ws_model.name,
            &self.session_id,
        );

        let mut rx = self
            .grpc
            .stream(
                &format!("{}/RawGetChatMessage", SVC),
                &req,
                &self.csrf_token,
                300000,
            )
            .await?;

        let mut full_text = String::new();
        while let Some(result) = rx.recv().await {
            match result {
                Ok(frame_data) => {
                    let parsed = parsers::parse_raw_response(&frame_data);
                    if parsed.is_error {
                        return Err(format!("RawGetChatMessage error: {}", parsed.text));
                    }
                    full_text = parsed.text;
                    if !parsed.in_progress {
                        break;
                    }
                }
                Err(e) => return Err(e),
            }
        }

        let output_tokens = (full_text.len() / 4) as u32;
        Ok(serde_json::json!({
            "id": request_id, "type": "message", "role": "assistant", "model": original_model,
            "content": [{"type": "text", "text": full_text}],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 }
        }))
    }
}
