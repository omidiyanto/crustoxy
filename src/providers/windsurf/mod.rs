//! Windsurf provider — native gRPC integration with the Windsurf language server.
//!
//! Auto-enabled when WINDSURF_API_KEY is set. Supports both:
//!   - Cascade flow (modern models with modelUid)
//!   - RawGetChatMessage (legacy models with enumValue only)

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

use self::grpc::default_csrf_token;
use self::ls::LanguageServer;
use self::parsers::{
    STEP_STATUS_DONE, STEP_STATUS_GENERATING, STEP_TYPE_PLANNER_RESPONSE, TrajectoryStatus,
};

/// gRPC service paths for the language server.
const SVC: &str = "/exa.language_server_pb.LanguageServerService";

/// Shared context for streaming calls — avoids passing many individual params.
struct StreamCtx<'a> {
    api_key: &'a str,
    ls: &'a Arc<Mutex<LanguageServer>>,
    session_id: &'a str,
    messages: Vec<(String, String)>,
    system_prompt: String,
    model: &'a models::WindsurfModel,
    request_id: &'a str,
    original_model: &'a str,
    input_tokens: u32,
    tx: &'a tokio::sync::mpsc::Sender<bytes::Bytes>,
}

/// The Windsurf provider, managing a language server and handling chat requests.
pub struct WindsurfProvider {
    api_key: String,
    ls: Arc<Mutex<LanguageServer>>,
    session_id: String,
    workspace_initialized: Arc<Mutex<bool>>,
}

/// Exchange a Codeium auth token (Firebase ID token) for a Windsurf API key.
/// Tries register.windsurf.com first, falls back to api.codeium.com.
pub async fn register_codeium_token(token: &str) -> Result<(String, String), String> {
    let body = serde_json::json!({ "firebase_id_token": token });
    let body_str = body.to_string();

    let new_url =
        "https://register.windsurf.com/exa.seat_management_pb.SeatManagementService/RegisterUser";
    let legacy_url = "https://api.codeium.com/register_user/";

    let client = reqwest::Client::new();

    for (url, source) in [(new_url, "new"), (legacy_url, "legacy")] {
        let res = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("Accept", "application/json")
            .header("User-Agent", "windsurf/1.9600.41")
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
                warn!("RegisterUser {source} returned HTTP {}", r.status());
            }
            Err(e) => {
                warn!("RegisterUser {source} failed: {}", e);
            }
        }
    }

    Err("RegisterUser failed on both register.windsurf.com and api.codeium.com".to_string())
}

impl WindsurfProvider {
    /// Initialize the Windsurf provider: spawn LS, init session.
    ///
    /// Pass `api_key` directly, or provide `auth_token` to exchange it for an API key
    /// via Codeium's registration endpoint.
    pub async fn new(
        api_key: Option<&str>,
        auth_token: Option<&str>,
        ls_path: &str,
        ls_port: u16,
        api_server_url: &str,
    ) -> Result<Self, String> {
        let resolved_api_key = match (api_key, auth_token) {
            (Some(key), _) => key.to_string(),
            (None, Some(token)) => {
                info!("CODEIUM_AUTH_TOKEN set, exchanging for Windsurf API key...");
                let (key, _server_url) = register_codeium_token(token).await?;
                key
            }
            (None, None) => return Err("Either api_key or auth_token must be provided".to_string()),
        };

        let csrf = default_csrf_token();

        let ls = LanguageServer::start(ls_path, ls_port, csrf, api_server_url).await?;
        let session_id = Uuid::new_v4().to_string();

        let provider = Self {
            api_key: resolved_api_key.clone(),
            ls: Arc::new(Mutex::new(ls)),
            session_id,
            workspace_initialized: Arc::new(Mutex::new(false)),
        };

        // Initialize workspace (panel state, workspace, trust, heartbeat)
        provider.ensure_workspace_init().await?;

        info!(
            "Windsurf provider initialized (api_key={}...)",
            &resolved_api_key[..8.min(resolved_api_key.len())]
        );
        Ok(provider)
    }

    /// Check if the language server is alive and ready.
    pub async fn is_healthy(&self) -> bool {
        let mut ls = self.ls.lock().await;
        ls.is_alive() && ls.is_ready()
    }

    /// Attempt to restart the language server (e.g. after a crash).
    pub async fn try_restart(&self) -> Result<(), String> {
        let mut ls = self.ls.lock().await;
        ls.restart().await?;
        drop(ls);
        // Re-initialize workspace after restart
        let mut initialized = self.workspace_initialized.lock().await;
        *initialized = false;
        drop(initialized);
        self.ensure_workspace_init().await
    }

    /// Gracefully stop the language server.
    pub async fn shutdown(&self) {
        let mut ls = self.ls.lock().await;
        ls.stop().await;
    }

    /// One-shot workspace initialization for Cascade flow.
    async fn ensure_workspace_init(&self) -> Result<(), String> {
        let mut initialized = self.workspace_initialized.lock().await;
        if *initialized {
            return Ok(());
        }

        let ls = self.ls.lock().await;
        let csrf = &ls.csrf_token;

        // 1. InitializeCascadePanelState
        let req = builders::build_initialize_panel_state_request(&self.api_key, &self.session_id);
        ls.session
            .unary(
                &format!("{}/InitializeCascadePanelState", SVC),
                &req,
                csrf,
                10000,
            )
            .await
            .map_err(|e| format!("InitializeCascadePanelState failed: {}", e))?;
        debug!("Windsurf: InitializeCascadePanelState OK");

        // 2. AddTrackedWorkspace — create a scaffold directory
        let workspace = "/tmp/windsurf-workspace";
        std::fs::create_dir_all(workspace).ok();
        let req = builders::build_add_tracked_workspace_request(workspace);
        ls.session
            .unary(&format!("{}/AddTrackedWorkspace", SVC), &req, csrf, 10000)
            .await
            .map_err(|e| format!("AddTrackedWorkspace failed: {}", e))?;
        debug!("Windsurf: AddTrackedWorkspace OK");

        // 3. UpdateWorkspaceTrust
        let req = builders::build_update_workspace_trust_request(&self.api_key, &self.session_id);
        ls.session
            .unary(&format!("{}/UpdateWorkspaceTrust", SVC), &req, csrf, 10000)
            .await
            .map_err(|e| format!("UpdateWorkspaceTrust failed: {}", e))?;
        debug!("Windsurf: UpdateWorkspaceTrust OK");

        // 4. Heartbeat
        let req = builders::build_heartbeat_request(&self.api_key, &self.session_id);
        ls.session
            .unary(&format!("{}/Heartbeat", SVC), &req, csrf, 10000)
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
    /// Takes `Arc<Self>` to enable auto-restart on connection errors.
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
            let result = Self::do_stream(
                &provider.api_key,
                &provider.ls,
                &provider.session_id,
                &request,
                input_tokens,
                &request_id,
                &tx,
            )
            .await;

            if let Err(e) = result {
                error!("Windsurf stream error: {}", e);
                // Auto-restart on connection errors
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
        api_key: &str,
        ls: &Arc<Mutex<LanguageServer>>,
        session_id: &str,
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
            api_key,
            ls,
            session_id,
            messages,
            system_prompt,
            model: ws_model,
            request_id,
            original_model,
            input_tokens,
            tx,
        };

        if ws_model.use_cascade() {
            Self::stream_cascade(&ctx).await?;
        } else {
            Self::stream_raw(&ctx).await?;
        }

        Ok(())
    }

    /// Stream via Cascade flow: StartCascade → SendUserCascadeMessage → poll
    async fn stream_cascade(ctx: &StreamCtx<'_>) -> Result<(), String> {
        let ls_guard = ctx.ls.lock().await;
        let csrf = ls_guard.csrf_token.clone();

        // 1. StartCascade → cascade_id
        let req = builders::build_start_cascade_request(ctx.api_key, ctx.session_id);
        let resp = ls_guard
            .session
            .unary(&format!("{}/StartCascade", SVC), &req, &csrf, 30000)
            .await?;
        let cascade_id = parsers::parse_start_cascade_response(&resp);
        if cascade_id.is_empty() {
            return Err("StartCascade returned empty cascade_id".to_string());
        }
        debug!("Windsurf [{}]: cascade_id={}", ctx.request_id, cascade_id);

        // 2. Build the user message text (flatten history into tagged format)
        let mut text_parts = Vec::new();
        if !ctx.system_prompt.is_empty() {
            text_parts.push(ctx.system_prompt.clone());
            text_parts.push(String::new());
        }
        for (role, content) in &ctx.messages {
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
            ctx.api_key,
            &cascade_id,
            &user_text,
            ctx.model.enum_value,
            ctx.model.model_uid,
            ctx.session_id,
        );
        ls_guard
            .session
            .unary(
                &format!("{}/SendUserCascadeMessage", SVC),
                &req,
                &csrf,
                30000,
            )
            .await?;
        debug!("Windsurf [{}]: SendUserCascadeMessage OK", ctx.request_id);

        // 4. Poll GetCascadeTrajectorySteps
        let mut step_offset: u64 = 0;
        let mut total_text = String::new();
        let mut last_text_len = 0;
        let poll_interval = std::time::Duration::from_millis(500);
        let max_wait = std::time::Duration::from_secs(600);
        let started = std::time::Instant::now();
        let mut stall_since = std::time::Instant::now();

        loop {
            if started.elapsed() > max_wait {
                return Err("Cascade timeout (600s)".to_string());
            }

            // Check trajectory status
            let status_req = builders::build_get_trajectory_request(&cascade_id);
            let status_resp = ls_guard
                .session
                .unary(
                    &format!("{}/GetCascadeTrajectory", SVC),
                    &status_req,
                    &csrf,
                    10000,
                )
                .await?;
            let status = parsers::parse_trajectory_status(&status_resp);

            // Get new steps
            let steps_req = builders::build_get_trajectory_steps_request(&cascade_id, step_offset);
            let steps_resp = ls_guard
                .session
                .unary(
                    &format!("{}/GetCascadeTrajectorySteps", SVC),
                    &steps_req,
                    &csrf,
                    10000,
                )
                .await?;
            let steps = parsers::parse_trajectory_steps(&steps_resp);

            for step in &steps {
                step_offset += 1;

                // Check for errors
                if !step.error_text.is_empty() {
                    return Err(format!("Cascade error: {}", step.error_text));
                }

                // Only process planner response steps that are generating or done
                if step.step_type != STEP_TYPE_PLANNER_RESPONSE {
                    continue;
                }
                if step.status != STEP_STATUS_GENERATING && step.status != STEP_STATUS_DONE {
                    continue;
                }

                // Emit new text as SSE content_block_delta events
                if !step.text.is_empty() && step.text.len() > last_text_len {
                    let new_text = &step.text[last_text_len..];
                    if !new_text.is_empty() {
                        let delta_event = crate::sse::format_content_delta(new_text, 0);
                        ctx.tx
                            .send(bytes::Bytes::from(delta_event))
                            .await
                            .map_err(|_| "Channel closed".to_string())?;
                        last_text_len = step.text.len();
                        total_text = step.text.clone();
                        stall_since = std::time::Instant::now();
                    }
                }
            }

            // Check if done
            if status == TrajectoryStatus::Idle && steps.is_empty() {
                break;
            }

            // Warm stall detection (25s of no progress)
            if stall_since.elapsed() > std::time::Duration::from_secs(25) && !total_text.is_empty()
            {
                warn!(
                    "Windsurf [{}]: warm stall detected after {}s",
                    ctx.request_id,
                    stall_since.elapsed().as_secs()
                );
                break;
            }

            tokio::time::sleep(poll_interval).await;
        }

        // Send message_stop event
        let output_tokens = (total_text.len() / 4) as u32; // rough estimate
        let stop_event = crate::sse::format_message_stop(
            ctx.request_id,
            ctx.original_model,
            ctx.input_tokens,
            output_tokens,
            "end_turn",
        );
        ctx.tx
            .send(bytes::Bytes::from(stop_event))
            .await
            .map_err(|_| "Channel closed".to_string())?;

        info!(
            "Windsurf [{}]: cascade complete, ~{} chars",
            ctx.request_id,
            total_text.len()
        );
        Ok(())
    }

    /// Stream via RawGetChatMessage (legacy, for enum-only models).
    async fn stream_raw(ctx: &StreamCtx<'_>) -> Result<(), String> {
        let ls_guard = ctx.ls.lock().await;
        let csrf = ls_guard.csrf_token.clone();

        let req = builders::build_raw_get_chat_message_request(
            ctx.api_key,
            &ctx.messages,
            &ctx.system_prompt,
            ctx.model.enum_value,
            ctx.model.name,
            ctx.session_id,
        );

        let mut rx = ls_guard
            .session
            .stream(&format!("{}/RawGetChatMessage", SVC), &req, &csrf, 300000)
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
                        let new_text = if last_text_len == 0 {
                            &parsed.text
                        } else {
                            &parsed.text[last_text_len..]
                        };
                        if !new_text.is_empty() {
                            let delta_event = crate::sse::format_content_delta(new_text, 0);
                            ctx.tx
                                .send(bytes::Bytes::from(delta_event))
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
                Err(e) => {
                    return Err(format!("RawGetChatMessage stream error: {}", e));
                }
            }
        }

        let output_tokens = (total_text.len() / 4) as u32;
        let stop_event = crate::sse::format_message_stop(
            ctx.request_id,
            ctx.original_model,
            ctx.input_tokens,
            output_tokens,
            "end_turn",
        );
        ctx.tx
            .send(bytes::Bytes::from(stop_event))
            .await
            .map_err(|_| "Channel closed".to_string())?;

        info!(
            "Windsurf [{}]: raw complete, ~{} chars",
            ctx.request_id,
            total_text.len()
        );
        Ok(())
    }

    /// Non-streaming response (collect all text, return as JSON).
    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> Result<serde_json::Value, String> {
        let (messages, system_prompt) = Self::prepare_messages(request);
        let model_name = &request.model;
        let ws_model = models::resolve_model(model_name)
            .ok_or_else(|| format!("Unknown Windsurf model: {}", model_name))?;

        let original_model = request.original_model.as_deref().unwrap_or(&request.model);

        let ls_guard = self.ls.lock().await;
        let csrf = ls_guard.csrf_token.clone();

        // Use RawGetChatMessage for non-streaming (simpler)
        let req = builders::build_raw_get_chat_message_request(
            &self.api_key,
            &messages,
            &system_prompt,
            ws_model.enum_value,
            ws_model.name,
            &self.session_id,
        );

        let mut rx = ls_guard
            .session
            .stream(&format!("{}/RawGetChatMessage", SVC), &req, &csrf, 300000)
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
            "id": request_id,
            "type": "message",
            "role": "assistant",
            "model": original_model,
            "content": [{"type": "text", "text": full_text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            }
        }))
    }
}
