use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio_stream::Stream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::Settings;
use crate::heuristic_tool_parser::HeuristicToolParser;
use crate::models::anthropic::MessagesRequest;
use crate::sse::SSEBuilder;

const LOGIN_URL: &str = "https://puter.com/login";
const ANTHROPIC_PROXY_URL: &str = "https://api.puter.com/puterai/anthropic/v1/messages";

// Token refresh interval: 23 hours (Puter tokens typically expire in ~24h)
const TOKEN_LIFETIME_SECS: u64 = 23 * 3600;
const LOGIN_MAX_RETRIES: u32 = 3;
const LOGIN_BASE_DELAY_MS: u64 = 2000;

struct CachedToken {
    token: String,
    obtained_at: Instant,
}

/// Track the block type by Puter's content_block index so we know how to route deltas.
#[derive(Debug, Clone, Copy, PartialEq)]
enum UpstreamBlockType {
    Text,
    Thinking,
    ToolUse,
    Other,
}

pub struct PuterProvider {
    client: Client,
    username: String,
    password: String,
    cached_token: Arc<RwLock<Option<CachedToken>>>,
}

impl PuterProvider {
    pub async fn new(credentials: &str, settings: &Settings) -> Result<Self, String> {
        let (username, password) = credentials
            .split_once(':')
            .ok_or_else(|| "PUTER_API_KEY must be in format 'username:password'".to_string())?;

        let client = Client::builder()
            .timeout(Duration::from_secs(settings.http_read_timeout))
            .connect_timeout(Duration::from_secs(settings.http_connect_timeout))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        info!("Puter provider created (login deferred to first request)");
        Ok(Self {
            client,
            username: username.to_string(),
            password: password.to_string(),
            cached_token: Arc::new(RwLock::new(None)),
        })
    }

    /// Build a clean Anthropic request body for forwarding to Puter.
    /// Strips crustoxy-internal/null fields and patches `model` to the actual provider model name.
    fn build_forward_body(request: &MessagesRequest) -> Value {
        let model_name = Settings::parse_model_name(
            request
                .resolved_provider_model
                .as_deref()
                .unwrap_or(&request.model),
        )
        .to_string();

        let mut body = serde_json::to_value(request).unwrap_or_else(|_| json!({}));
        if let Some(obj) = body.as_object_mut() {
            obj.insert("model".to_string(), json!(model_name));
            obj.remove("extra_body");
            // Strip null fields to keep the forwarded body clean
            let null_keys: Vec<String> = obj
                .iter()
                .filter_map(|(k, v)| if v.is_null() { Some(k.clone()) } else { None })
                .collect();
            for k in null_keys {
                obj.remove(&k);
            }
        }
        body
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> impl Stream<Item = String> + use<> {
        let request_model = request.model.clone();
        let mut forward_body = Self::build_forward_body(request);
        if let Some(obj) = forward_body.as_object_mut() {
            obj.insert("stream".to_string(), json!(true));
        }

        let model_for_log = forward_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let msg_count = forward_body
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let tool_count = forward_body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        let client = self.client.clone();
        let cached_token = self.cached_token.clone();
        let username = self.username.clone();
        let password = self.password.clone();
        let request_id = request_id.to_string();

        info!(
            "PUTER_STREAM: request_id={} model={} msgs={} tools={}",
            request_id, model_for_log, msg_count, tool_count,
        );

        async_stream::stream! {
            let message_id = format!("msg_{}", Uuid::new_v4());
            let mut sse = SSEBuilder::new(message_id, request_model.clone(), input_tokens);
            yield sse.message_start();

            let mut attempts: u32 = 0;
            let max_attempts: u32 = 2;

            'retry: loop {
                attempts += 1;
                if attempts > max_attempts {
                    for event in sse.emit_error("Puter: auth retry exhausted") {
                        yield event;
                    }
                    yield sse.message_delta("end_turn", 0);
                    yield sse.message_stop();
                    return;
                }

                let token = match ensure_token(&client, &cached_token, &username, &password).await {
                    Ok(t) => t,
                    Err(e) => {
                        error!("Puter auth failed: {}", e);
                        for event in sse.emit_error(&format!("Puter auth failed: {}", e)) {
                            yield event;
                        }
                        yield sse.message_delta("end_turn", 0);
                        yield sse.message_stop();
                        return;
                    }
                };

                let resp = client
                    .post(ANTHROPIC_PROXY_URL)
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("anthropic-version", "2023-06-01")
                    .header("Accept", "text/event-stream")
                    .json(&forward_body)
                    .send()
                    .await;

                let response = match resp {
                    Err(e) => {
                        error!("PUTER_STREAM_ERROR: request_id={} error={}", request_id, e);
                        for event in sse.emit_error(&format!("Puter connection error: {}", e)) {
                            yield event;
                        }
                        yield sse.message_delta("end_turn", 0);
                        yield sse.message_stop();
                        return;
                    }
                    Ok(r) => r,
                };

                let status = response.status().as_u16();

                if status == 401 || status == 403 {
                    warn!("Puter auth error ({}), invalidating token and retrying...", status);
                    let mut cached = cached_token.write().await;
                    *cached = None;
                    drop(cached);
                    continue 'retry;
                }

                if status >= 400 {
                    let body_text = response.text().await.unwrap_or_default();
                    error!("Puter API error {}: {}", status, body_text);
                    let trunc = &body_text[..body_text.len().min(500)];
                    for event in sse.emit_error(&format!("Puter error {}: {}", status, trunc)) {
                        yield event;
                    }
                    yield sse.message_delta("end_turn", 0);
                    yield sse.message_stop();
                    return;
                }

                // ── Parse Puter's Anthropic SSE stream ──
                // We parse each event and re-emit using our SSEBuilder so we can:
                // 1. Intercept text_delta and run HeuristicToolParser (for models
                //    that emit raw `functions.Name:N{json}` tool calls).
                // 2. Keep our block indices consistent when inserting tool_use blocks.
                let mut heuristic_parser = HeuristicToolParser::new();
                let mut had_tool_call = false;
                let mut finish_reason: Option<String> = None;
                let mut usage_output_tokens: Option<u32> = None;
                // Maps Puter's upstream content_block index → our local block type
                let mut upstream_block_types: HashMap<u64, UpstreamBlockType> = HashMap::new();
                // Track tool index → sse tool_index (for native tool_use passthrough)
                let mut upstream_tool_index_map: HashMap<u64, i32> = HashMap::new();
                let mut next_tool_index: i32 = 0;

                let mut byte_stream = response.bytes_stream();
                let mut line_buffer = String::new();
                let mut current_event: Option<String> = None;

                while let Some(chunk_result) = byte_stream.next().await {
                    let bytes = match chunk_result {
                        Ok(b) => b,
                        Err(e) => {
                            error!("Puter stream read error: {}", e);
                            break;
                        }
                    };

                    line_buffer.push_str(&String::from_utf8_lossy(&bytes));

                    while let Some(newline_pos) = line_buffer.find('\n') {
                        let line = line_buffer[..newline_pos].trim_end_matches('\r').to_string();
                        line_buffer = line_buffer[newline_pos + 1..].to_string();

                        if line.is_empty() {
                            // End of SSE event; reset current_event
                            current_event = None;
                            continue;
                        }

                        if let Some(name) = line.strip_prefix("event: ") {
                            current_event = Some(name.to_string());
                            continue;
                        }

                        let Some(data_str) = line.strip_prefix("data: ") else {
                            continue;
                        };

                        let data: Value = match serde_json::from_str(data_str) {
                            Ok(v) => v,
                            Err(e) => {
                                debug!("Puter: skipping unparseable SSE data: {} ({})", data_str, e);
                                continue;
                            }
                        };

                        let event_name = current_event.as_deref().unwrap_or_else(|| {
                            data.get("type").and_then(|v| v.as_str()).unwrap_or("")
                        });

                        match event_name {
                            "message_start" => {
                                // Ignore — we emit our own message_start at the top
                            }
                            "content_block_start" => {
                                let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let block = data.get("content_block").cloned().unwrap_or(json!({}));
                                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

                                match block_type {
                                    "text" => {
                                        upstream_block_types.insert(idx, UpstreamBlockType::Text);
                                        // Lazy: only create local text block when first delta arrives
                                    }
                                    "thinking" => {
                                        upstream_block_types.insert(idx, UpstreamBlockType::Thinking);
                                        // Lazy: only create local thinking block when first delta arrives
                                    }
                                    "tool_use" => {
                                        upstream_block_types.insert(idx, UpstreamBlockType::ToolUse);
                                        had_tool_call = true;
                                        let tool_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let tool_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();

                                        // Close any open text/thinking block before a native tool_use
                                        for event in sse.close_content_blocks() {
                                            yield event;
                                        }

                                        let my_idx = next_tool_index;
                                        next_tool_index += 1;
                                        upstream_tool_index_map.insert(idx, my_idx);
                                        sse.blocks.register_tool_name(my_idx, &tool_name);
                                        yield sse.start_tool_block(my_idx, &tool_id, &tool_name);
                                    }
                                    _ => {
                                        upstream_block_types.insert(idx, UpstreamBlockType::Other);
                                    }
                                }
                            }
                            "content_block_delta" => {
                                let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let delta = data.get("delta").cloned().unwrap_or(json!({}));
                                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                let block_type = upstream_block_types.get(&idx).copied().unwrap_or(UpstreamBlockType::Other);

                                match (block_type, delta_type) {
                                    (UpstreamBlockType::Text, "text_delta") => {
                                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                            let (filtered_text, detected_tools) = heuristic_parser.feed(text);

                                            if !filtered_text.is_empty() {
                                                for event in sse.ensure_text_block() {
                                                    yield event;
                                                }
                                                yield sse.emit_text_delta(&filtered_text);
                                            }

                                            for tool_use in &detected_tools {
                                                had_tool_call = true;
                                                for event in sse.close_content_blocks() {
                                                    yield event;
                                                }
                                                let block_idx = sse.blocks.allocate_index();
                                                let mut input = tool_use.input.clone();
                                                if tool_use.name == "Task" {
                                                    input.insert(
                                                        "run_in_background".to_string(),
                                                        "false".to_string(),
                                                    );
                                                }
                                                let input_json = serde_json::to_string(&input)
                                                    .unwrap_or_else(|_| "{}".to_string());
                                                yield sse.content_block_start(
                                                    block_idx,
                                                    "tool_use",
                                                    json!({"id": tool_use.id, "name": tool_use.name}),
                                                );
                                                yield sse.content_block_delta(
                                                    block_idx,
                                                    "input_json_delta",
                                                    &input_json,
                                                );
                                                yield sse.content_block_stop(block_idx);
                                            }
                                        }
                                    }
                                    (UpstreamBlockType::Thinking, "thinking_delta") => {
                                        if let Some(text) = delta.get("thinking").and_then(|v| v.as_str())
                                            && !text.is_empty()
                                        {
                                            for event in sse.ensure_thinking_block() {
                                                yield event;
                                            }
                                            yield sse.emit_thinking_delta(text);
                                        }
                                    }
                                    (UpstreamBlockType::Thinking, "signature_delta") => {
                                        // Signature is metadata for verified thinking; ignore
                                    }
                                    (UpstreamBlockType::ToolUse, "input_json_delta") => {
                                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str())
                                            && let Some(&my_idx) = upstream_tool_index_map.get(&idx)
                                        {
                                            yield sse.emit_tool_delta(my_idx, partial);
                                        }
                                    }
                                    _ => {
                                        debug!(
                                            "Puter: skipping unhandled delta (block_type={:?}, delta_type={})",
                                            block_type, delta_type
                                        );
                                    }
                                }
                            }
                            "content_block_stop" => {
                                let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let block_type = upstream_block_types.get(&idx).copied().unwrap_or(UpstreamBlockType::Other);

                                match block_type {
                                    UpstreamBlockType::ToolUse => {
                                        if let Some(&my_idx) = upstream_tool_index_map.get(&idx)
                                            && let Some(state) = sse.blocks.tool_states.get(&my_idx)
                                            && state.started
                                        {
                                            let block_idx = state.block_index as u32;
                                            yield sse.content_block_stop(block_idx);
                                            if let Some(state) = sse.blocks.tool_states.get_mut(&my_idx) {
                                                state.started = false;
                                            }
                                        }
                                    }
                                    UpstreamBlockType::Text | UpstreamBlockType::Thinking => {
                                        // Close our local blocks (will be idempotent at final flush)
                                        for event in sse.close_content_blocks() {
                                            yield event;
                                        }
                                    }
                                    UpstreamBlockType::Other => {}
                                }
                            }
                            "message_delta" => {
                                if let Some(reason) = data.get("delta")
                                    .and_then(|d| d.get("stop_reason"))
                                    .and_then(|v| v.as_str())
                                {
                                    finish_reason = Some(reason.to_string());
                                }
                                if let Some(tokens) = data.get("usage")
                                    .and_then(|u| u.get("output_tokens"))
                                    .and_then(|v| v.as_u64())
                                {
                                    usage_output_tokens = Some(tokens as u32);
                                }
                            }
                            "message_stop" => {
                                // Will emit our own after loop
                            }
                            "error" => {
                                let msg = data.get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown Puter error");
                                error!("Puter stream error: {}", msg);
                                for event in sse.emit_error(&format!("Puter: {}", msg)) {
                                    yield event;
                                }
                            }
                            "ping" => {
                                // Ignore keepalive pings
                            }
                            _ => {
                                debug!("Puter: ignoring unknown event type: {}", event_name);
                            }
                        }
                    }
                }

                // Flush heuristic tool parser for any trailing tool calls
                for tool_use in heuristic_parser.flush() {
                    had_tool_call = true;
                    for event in sse.close_content_blocks() {
                        yield event;
                    }
                    let block_idx = sse.blocks.allocate_index();
                    let mut input = tool_use.input.clone();
                    if tool_use.name == "Task" {
                        input.insert("run_in_background".to_string(), "false".to_string());
                    }
                    let input_json = serde_json::to_string(&input)
                        .unwrap_or_else(|_| "{}".to_string());
                    yield sse.content_block_start(
                        block_idx,
                        "tool_use",
                        json!({"id": tool_use.id, "name": tool_use.name}),
                    );
                    yield sse.content_block_delta(
                        block_idx,
                        "input_json_delta",
                        &input_json,
                    );
                    yield sse.content_block_stop(block_idx);
                }

                if !sse.has_any_content() {
                    for event in sse.ensure_text_block() {
                        yield event;
                    }
                    yield sse.emit_text_delta(" ");
                }

                for event in sse.close_all_blocks() {
                    yield event;
                }

                let output_tokens = usage_output_tokens.unwrap_or_else(|| sse.estimate_output_tokens());
                let stop_reason = if had_tool_call {
                    "tool_use"
                } else {
                    match finish_reason.as_deref() {
                        Some("tool_use") => "tool_use",
                        Some("max_tokens") => "max_tokens",
                        Some("stop_sequence") => "stop_sequence",
                        _ => "end_turn",
                    }
                };
                yield sse.message_delta(stop_reason, output_tokens);
                yield sse.message_stop();
                return;
            }
        }
    }

    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        _input_tokens: u32,
        request_id: &str,
    ) -> Result<Value, String> {
        let mut forward_body = Self::build_forward_body(request);
        if let Some(obj) = forward_body.as_object_mut() {
            obj.insert("stream".to_string(), json!(false));
        }

        let model_for_log = forward_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        info!(
            "PUTER_NON_STREAM: request_id={} model={}",
            request_id, model_for_log
        );

        let mut attempts: u32 = 0;
        loop {
            attempts += 1;
            if attempts > 2 {
                return Err("Puter: auth retry exhausted".to_string());
            }

            let token = ensure_token(
                &self.client,
                &self.cached_token,
                &self.username,
                &self.password,
            )
            .await?;

            let response = self
                .client
                .post(ANTHROPIC_PROXY_URL)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", token))
                .header("anthropic-version", "2023-06-01")
                .json(&forward_body)
                .send()
                .await
                .map_err(|e| format!("Puter connection error: {}", e))?;

            let status = response.status().as_u16();

            if status == 401 || status == 403 {
                warn!(
                    "Puter auth error ({}), invalidating token and retrying...",
                    status
                );
                let mut cached = self.cached_token.write().await;
                *cached = None;
                drop(cached);
                continue;
            }

            if status >= 400 {
                let body_text = response.text().await.unwrap_or_default();
                let trunc = &body_text[..body_text.len().min(500)];
                return Err(format!("Puter error {}: {}", status, trunc));
            }

            return response
                .json::<Value>()
                .await
                .map_err(|e| format!("Puter response parse error: {}", e));
        }
    }
}

/// Obtain a valid token from cache or login.
async fn ensure_token(
    client: &Client,
    cached_token: &Arc<RwLock<Option<CachedToken>>>,
    username: &str,
    password: &str,
) -> Result<String, String> {
    {
        let cached = cached_token.read().await;
        if let Some(ref ct) = *cached
            && ct.obtained_at.elapsed().as_secs() < TOKEN_LIFETIME_SECS
        {
            return Ok(ct.token.clone());
        }
    }

    let token = do_login(client, username, password).await?;
    {
        let mut cached = cached_token.write().await;
        *cached = Some(CachedToken {
            token: token.clone(),
            obtained_at: Instant::now(),
        });
    }
    Ok(token)
}

/// Login to Puter with exponential backoff retry.
async fn do_login(client: &Client, username: &str, password: &str) -> Result<String, String> {
    let mut last_err = String::new();

    for attempt in 1..=LOGIN_MAX_RETRIES {
        info!(
            "Puter: login attempt {}/{} as '{}'...",
            attempt, LOGIN_MAX_RETRIES, username
        );

        let payload = json!({
            "username": username,
            "password": password,
        });

        let result = client
            .post(LOGIN_URL)
            .header("Content-Type", "application/json")
            .header("Origin", "https://puter.com")
            .header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64; rv:149.0) Gecko/20100101 Firefox/149.0",
            )
            .json(&payload)
            .send()
            .await;

        match result {
            Err(e) => {
                last_err = format!("Puter login request failed: {}", e);
                warn!(
                    "Puter login attempt {}/{} failed: {}",
                    attempt, LOGIN_MAX_RETRIES, last_err
                );
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body: Value = response
                    .json()
                    .await
                    .map_err(|e| format!("Puter login response parse error: {}", e))?;

                if status >= 400 {
                    let msg = body
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error");
                    last_err = format!("Puter login failed ({}): {}", status, msg);
                    if status == 401 || status == 403 {
                        return Err(last_err);
                    }
                    warn!(
                        "Puter login attempt {}/{} failed: {}",
                        attempt, LOGIN_MAX_RETRIES, last_err
                    );
                } else {
                    let token = body
                        .get("token")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "Puter login response missing 'token' field".to_string())?;
                    info!("Puter: login successful on attempt {}", attempt);
                    return Ok(token.to_string());
                }
            }
        }

        if attempt < LOGIN_MAX_RETRIES {
            let delay = LOGIN_BASE_DELAY_MS * 2u64.pow(attempt - 1);
            let jitter = rand::random::<u64>() % (delay / 2);
            let total = delay + jitter;
            info!("Puter: retrying login in {}ms...", total);
            tokio::time::sleep(Duration::from_millis(total)).await;
        }
    }

    Err(format!(
        "Puter login failed after {} attempts: {}",
        LOGIN_MAX_RETRIES, last_err
    ))
}
