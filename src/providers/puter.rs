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
use crate::converter::{build_openai_request, map_stop_reason};
use crate::heuristic_tool_parser::HeuristicToolParser;
use crate::models::anthropic::MessagesRequest;
use crate::models::openai::ChatMessage;
use crate::sse::SSEBuilder;
use crate::think_parser::{ContentType, ThinkTagParser};
use crate::tool_intent_detector;

const LOGIN_URL: &str = "https://puter.com/login";
const DRIVERS_CALL_URL: &str = "https://api.puter.com/drivers/call";

// Token refresh interval: 23 hours (Puter tokens typically expire in ~24h)
const TOKEN_LIFETIME_SECS: u64 = 23 * 3600;
const LOGIN_MAX_RETRIES: u32 = 3;
const LOGIN_BASE_DELAY_MS: u64 = 2000;

struct CachedToken {
    token: String,
    obtained_at: Instant,
}

pub struct PuterProvider {
    client: Client,
    username: String,
    password: String,
    cached_token: Arc<RwLock<Option<CachedToken>>>,
    enable_tool_retry: bool,
    tool_retry_max: u32,
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
            enable_tool_retry: settings.enable_tool_retry,
            tool_retry_max: settings.tool_retry_max,
        })
    }

    /// Wrap an OpenAI-style chat request body in Puter's `/drivers/call` envelope.
    fn build_envelope(token: &str, args: Value) -> Value {
        json!({
            "interface": "puter-chat-completion",
            "driver": "ai-chat",
            "test_mode": false,
            "method": "complete",
            "args": args,
            "auth_token": token,
        })
    }

    /// Build the `args` object for Puter's chat driver — full OpenAI-style request
    /// (preserves messages with tool_calls/tool_call_id, tools, tool_choice).
    fn build_args(request: &MessagesRequest, stream: bool) -> Value {
        let model_name = Settings::parse_model_name(
            request
                .resolved_provider_model
                .as_deref()
                .unwrap_or(&request.model),
        )
        .to_string();

        let openai_req = build_openai_request(request, &model_name);
        let mut args = serde_json::to_value(&openai_req).unwrap_or_else(|_| json!({}));
        if let Some(obj) = args.as_object_mut() {
            obj.insert("stream".to_string(), json!(stream));
            // stream_options is OpenAI-specific; Puter driver doesn't need it
            obj.remove("stream_options");
        }
        args
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> impl Stream<Item = String> + use<> {
        let request_model = request.model.clone();
        let initial_args = Self::build_args(request, true);

        let model_for_log = initial_args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let msg_count = initial_args
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let tool_count = initial_args
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        let client = self.client.clone();
        let cached_token = self.cached_token.clone();
        let username = self.username.clone();
        let password = self.password.clone();
        let request_id = request_id.to_string();
        let enable_tool_retry = self.enable_tool_retry;
        let tool_retry_max = self.tool_retry_max;

        info!(
            "PUTER_STREAM: request_id={} model={} msgs={} tools={}",
            request_id, model_for_log, msg_count, tool_count,
        );

        async_stream::stream! {
            let message_id = format!("msg_{}", Uuid::new_v4());
            let mut sse = SSEBuilder::new(message_id, request_model.clone(), input_tokens);
            yield sse.message_start();

            let max_http_retries: u32 = 2;
            let max_tool_attempts: u32 = if enable_tool_retry { tool_retry_max + 1 } else { 1 };
            let mut tool_attempt: u32 = 0;
            let mut current_args = initial_args.clone();
            let mut accumulated_all_text = String::new();
            let mut had_tool_call = false;
            let mut final_finish_reason: Option<String> = None;
            let mut final_output_tokens: Option<u32> = None;
            let mut last_error: Option<String> = None;

            'tool_retry: while tool_attempt < max_tool_attempts {
                let mut http_attempt: u32 = 0;
                let mut auth_attempts: u32 = 0;

                'http_retry: loop {
                    // Obtain a token (login if necessary)
                    let token = match ensure_token(&client, &cached_token, &username, &password).await {
                        Ok(t) => t,
                        Err(e) => {
                            last_error = Some(format!("Puter auth failed: {}", e));
                            break 'http_retry;
                        }
                    };

                    let envelope = Self::build_envelope(&token, current_args.clone());

                    let resp = client
                        .post(DRIVERS_CALL_URL)
                        .header("Content-Type", "application/json")
                        .header("Origin", "https://puter.com")
                        .header(
                            "User-Agent",
                            "Mozilla/5.0 (X11; Linux x86_64; rv:149.0) Gecko/20100101 Firefox/149.0",
                        )
                        .json(&envelope)
                        .send()
                        .await;

                    let response = match resp {
                        Err(e) => {
                            error!("PUTER_STREAM_ERROR: request_id={} attempt={} error={}", request_id, http_attempt, e);
                            last_error = Some(format!("Puter connection error: {}", e));
                            if http_attempt < max_http_retries {
                                http_attempt += 1;
                                let delay = 2u64.pow(http_attempt);
                                tokio::time::sleep(Duration::from_secs(delay)).await;
                                continue 'http_retry;
                            }
                            break 'http_retry;
                        }
                        Ok(r) => r,
                    };

                    let status = response.status().as_u16();

                    // Auth failure → invalidate token and retry once
                    if (status == 401 || status == 403) && auth_attempts < 1 {
                        warn!("Puter auth error ({}), invalidating token...", status);
                        let mut cached = cached_token.write().await;
                        *cached = None;
                        drop(cached);
                        auth_attempts += 1;
                        continue 'http_retry;
                    }

                    if status >= 400 {
                        let body_text = response.text().await.unwrap_or_default();
                        let trunc = &body_text[..body_text.len().min(500)];
                        error!("Puter API error {}: {}", status, trunc);
                        last_error = Some(format!("Puter error {}: {}", status, trunc));
                        if status >= 500 && http_attempt < max_http_retries {
                            http_attempt += 1;
                            let delay = 2u64.pow(http_attempt);
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            continue 'http_retry;
                        }
                        break 'http_retry;
                    }

                    debug!(
                        "PUTER_DEBUG[{}] stream started: tool_attempt={}/{}",
                        request_id, tool_attempt + 1, max_tool_attempts
                    );
                    // ── Successful response: parse stream ──
                    // Puter `/drivers/call` returns newline-delimited JSON for streams.
                    // Each line is a chunk like:
                    //   {"type": "text", "text": "..."}
                    //   {"type": "reasoning", "reasoning": "..."}
                    //   {"type": "error", "error": "..."}
                    // The final line may include `finish_reason` and/or `usage`.
                    let mut think_parser = ThinkTagParser::new();
                    let mut heuristic_parser = HeuristicToolParser::new();
                    let mut finish_reason: Option<String> = None;
                    let mut usage_output_tokens: Option<u32> = None;
                    let mut attempt_text = String::new();
                    let mut attempt_had_tool = false;
                    let mut byte_stream = response.bytes_stream();
                    let mut line_buffer = String::new();
                    let mut stream_failed = false;
                    let mut chunk_count: u32 = 0;
                    let mut chunk_type_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

                    'stream: while let Some(chunk_result) = byte_stream.next().await {
                        let bytes = match chunk_result {
                            Ok(b) => b,
                            Err(e) => {
                                error!("Puter stream read error: {}", e);
                                last_error = Some(format!("Puter stream read error: {}", e));
                                stream_failed = true;
                                break 'stream;
                            }
                        };

                        line_buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(newline_pos) = line_buffer.find('\n') {
                            let line = line_buffer[..newline_pos].trim().to_string();
                            line_buffer = line_buffer[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            let chunk: Value = match serde_json::from_str(&line) {
                                Ok(v) => v,
                                Err(e) => {
                                    debug!(
                                        "PUTER_DEBUG[{}] UNPARSEABLE_LINE: {} (err: {})",
                                        request_id,
                                        truncate_for_log(&line, 300),
                                        e
                                    );
                                    continue;
                                }
                            };
                            chunk_count += 1;

                            // Track final-chunk metadata if present
                            if let Some(reason) = chunk.get("finish_reason").and_then(|v| v.as_str()) {
                                finish_reason = Some(reason.to_string());
                            }
                            // Puter usage shape: {"usage": {"prompt": N, "completion": N, ...}}
                            // Also tolerate OpenAI-style {"completion_tokens": N, "output_tokens": N}.
                            if let Some(usage_obj) = chunk.get("usage") {
                                let tokens = usage_obj
                                    .get("completion")
                                    .or_else(|| usage_obj.get("completion_tokens"))
                                    .or_else(|| usage_obj.get("output_tokens"))
                                    .and_then(|v| v.as_u64());
                                if let Some(t) = tokens {
                                    usage_output_tokens = Some(t as u32);
                                }
                            }

                            let chunk_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("<no-type>");
                            *chunk_type_counts.entry(chunk_type.to_string()).or_insert(0) += 1;

                            // Log every chunk for debugging (truncated to keep logs readable)
                            debug!(
                                "PUTER_DEBUG[{}] chunk#{} type={} raw={}",
                                request_id,
                                chunk_count,
                                chunk_type,
                                truncate_for_log(&line, 400)
                            );

                            match chunk_type {
                                "text" => {
                                    let Some(text) = chunk.get("text").and_then(|v| v.as_str()) else {
                                        continue;
                                    };
                                    attempt_text.push_str(text);
                                    let think_chunks = think_parser.feed(text);
                                    for c in think_chunks {
                                        match c.content_type {
                                            ContentType::Thinking => {
                                                for event in sse.ensure_thinking_block() {
                                                    yield event;
                                                }
                                                yield sse.emit_thinking_delta(&c.content);
                                            }
                                            ContentType::Text => {
                                                let (filtered_text, detected_tools) =
                                                    heuristic_parser.feed(&c.content);

                                                if !filtered_text.is_empty() {
                                                    for event in sse.ensure_text_block() {
                                                        yield event;
                                                    }
                                                    yield sse.emit_text_delta(&filtered_text);
                                                }

                                                for tool_use in &detected_tools {
                                                    attempt_had_tool = true;
                                                    debug!(
                                                        "PUTER_DEBUG[{}] HEURISTIC_TOOL_DETECTED name={} keys=[{}]",
                                                        request_id,
                                                        tool_use.name,
                                                        tool_use.input.keys().cloned().collect::<Vec<_>>().join(",")
                                                    );
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
                                    }
                                }
                                "reasoning" => {
                                    if let Some(reasoning) = chunk.get("reasoning").and_then(|v| v.as_str())
                                        && !reasoning.is_empty()
                                    {
                                        for event in sse.ensure_thinking_block() {
                                            yield event;
                                        }
                                        yield sse.emit_thinking_delta(reasoning);
                                    }
                                }
                                "tool_use" => {
                                    // Puter native tool_use chunk:
                                    // {"type":"tool_use","id":"call_...","name":"Write","input":{...},"text":""}
                                    let tool_id = chunk
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| format!("toolu_{}", Uuid::new_v4()));
                                    let tool_name = chunk
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let mut input_val = chunk.get("input").cloned().unwrap_or(json!({}));

                                    // Force Task subagent to foreground
                                    if tool_name == "Task"
                                        && let Some(obj) = input_val.as_object_mut()
                                    {
                                        obj.insert("run_in_background".to_string(), json!(false));
                                    }

                                    let input_keys: Vec<String> = input_val
                                        .as_object()
                                        .map(|o| o.keys().cloned().collect())
                                        .unwrap_or_default();
                                    debug!(
                                        "PUTER_DEBUG[{}] NATIVE_TOOL_USE name={} id={} keys=[{}]",
                                        request_id,
                                        tool_name,
                                        tool_id,
                                        input_keys.join(",")
                                    );

                                    attempt_had_tool = true;
                                    for event in sse.close_content_blocks() {
                                        yield event;
                                    }
                                    let block_idx = sse.blocks.allocate_index();
                                    let input_json = serde_json::to_string(&input_val)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    yield sse.content_block_start(
                                        block_idx,
                                        "tool_use",
                                        json!({"id": tool_id, "name": tool_name}),
                                    );
                                    yield sse.content_block_delta(
                                        block_idx,
                                        "input_json_delta",
                                        &input_json,
                                    );
                                    yield sse.content_block_stop(block_idx);
                                }
                                "usage" => {
                                    // Already extracted above into usage_output_tokens.
                                    // No additional emission needed.
                                }
                                "error" => {
                                    let msg = chunk
                                        .get("error")
                                        .and_then(|e| {
                                            if let Some(s) = e.as_str() {
                                                Some(s.to_string())
                                            } else {
                                                e.get("message")
                                                    .and_then(|m| m.as_str())
                                                    .map(|s| s.to_string())
                                            }
                                        })
                                        .unwrap_or_else(|| "Unknown Puter error".to_string());
                                    error!("Puter stream error: {}", msg);
                                    last_error = Some(format!("Puter: {}", msg));
                                    stream_failed = true;
                                    break 'stream;
                                }
                                _ => {
                                    debug!("Puter: ignoring unknown chunk type: {}", chunk_type);
                                }
                            }
                        }
                    }

                    // Flush think parser
                    if let Some(remaining) = think_parser.flush() {
                        match remaining.content_type {
                            ContentType::Thinking => {
                                for event in sse.ensure_thinking_block() {
                                    yield event;
                                }
                                yield sse.emit_thinking_delta(&remaining.content);
                            }
                            ContentType::Text => {
                                attempt_text.push_str(&remaining.content);
                                let (filtered_text, detected_tools) =
                                    heuristic_parser.feed(&remaining.content);
                                if !filtered_text.is_empty() {
                                    for event in sse.ensure_text_block() {
                                        yield event;
                                    }
                                    yield sse.emit_text_delta(&filtered_text);
                                }
                                for tool_use in &detected_tools {
                                    attempt_had_tool = true;
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
                    }

                    // Flush heuristic tool parser for any trailing tool calls
                    for tool_use in heuristic_parser.flush() {
                        attempt_had_tool = true;
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

                    // Track results for retry decision
                    accumulated_all_text.push_str(&attempt_text);
                    if attempt_had_tool {
                        had_tool_call = true;
                    }
                    final_finish_reason = finish_reason.clone();
                    final_output_tokens = usage_output_tokens;

                    // ── DEBUG: log full stream summary ──
                    let type_summary = chunk_type_counts
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    debug!(
                        "PUTER_DEBUG[{}] STREAM_END chunks={} types=[{}] text_len={} had_tool={} stream_failed={} finish_reason={:?} output_tokens={:?}",
                        request_id,
                        chunk_count,
                        type_summary,
                        attempt_text.len(),
                        attempt_had_tool,
                        stream_failed,
                        finish_reason,
                        usage_output_tokens
                    );
                    if !attempt_text.is_empty() {
                        debug!(
                            "PUTER_DEBUG[{}] ACCUMULATED_TEXT (first 1000 chars): {}",
                            request_id,
                            truncate_for_log(&attempt_text, 1000)
                        );
                    }

                    // ── POST-STREAM: tool intent retry ──
                    let intent_detected = tool_intent_detector::has_tool_intent(&accumulated_all_text);
                    debug!(
                        "PUTER_DEBUG[{}] retry_check: had_tool={} intent_detected={} enable_retry={} tool_attempt={}/{}",
                        request_id, had_tool_call, intent_detected, enable_tool_retry,
                        tool_attempt + 1, max_tool_attempts
                    );
                    if !had_tool_call
                        && enable_tool_retry
                        && tool_attempt + 1 < max_tool_attempts
                        && intent_detected
                    {
                        info!(
                            "PUTER_TOOL_RETRY: request_id={} attempt={}/{} — Tool intent detected but no tool call produced. Retrying...",
                            request_id, tool_attempt + 1, max_tool_attempts - 1
                        );

                        for event in sse.close_content_blocks() {
                            yield event;
                        }

                        // Append assistant text + corrective user prompt
                        if let Some(messages) = current_args.get_mut("messages")
                            .and_then(|v| v.as_array_mut())
                        {
                            let assistant_msg = ChatMessage {
                                role: "assistant".to_string(),
                                content: Some(attempt_text.clone()),
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            };
                            let retry_msg = ChatMessage {
                                role: "user".to_string(),
                                content: Some(tool_intent_detector::RETRY_PROMPT.to_string()),
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            };
                            messages.push(serde_json::to_value(&assistant_msg).unwrap_or(json!({})));
                            messages.push(serde_json::to_value(&retry_msg).unwrap_or(json!({})));
                        }

                        tool_attempt += 1;
                        last_error = None;
                        continue 'tool_retry;
                    }

                    // No tool retry — finalize this HTTP attempt
                    if !stream_failed {
                        last_error = None;
                    } else if http_attempt < max_http_retries {
                        http_attempt += 1;
                        let delay = 2u64.pow(http_attempt);
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        continue 'http_retry;
                    }
                    break 'http_retry;
                }

                if last_error.is_some() {
                    break 'tool_retry;
                }

                // Successful path — fall through to finalize
                if !sse.has_any_content() {
                    for event in sse.ensure_text_block() {
                        yield event;
                    }
                    yield sse.emit_text_delta(" ");
                }

                for event in sse.close_all_blocks() {
                    yield event;
                }

                let output_tokens = final_output_tokens.unwrap_or_else(|| sse.estimate_output_tokens());
                let stop_reason = if had_tool_call {
                    "tool_use"
                } else {
                    map_stop_reason(final_finish_reason.as_deref())
                };
                yield sse.message_delta(stop_reason, output_tokens);
                yield sse.message_stop();
                return;
            }

            // Error path
            let error_msg = last_error.unwrap_or_else(|| "Unknown Puter error".to_string());
            for event in sse.close_content_blocks() {
                yield event;
            }
            for event in sse.emit_error(&format!("Error: {}", error_msg)) {
                yield event;
            }
            for event in sse.close_all_blocks() {
                yield event;
            }
            yield sse.message_delta("end_turn", 0);
            yield sse.message_stop();
        }
    }

    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> Result<Value, String> {
        let args = Self::build_args(request, false);
        let model_for_log = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        info!(
            "PUTER_NON_STREAM: request_id={} model={}",
            request_id, model_for_log
        );

        let mut auth_attempts: u32 = 0;
        loop {
            let token = ensure_token(
                &self.client,
                &self.cached_token,
                &self.username,
                &self.password,
            )
            .await?;

            let envelope = Self::build_envelope(&token, args.clone());

            let response = self
                .client
                .post(DRIVERS_CALL_URL)
                .header("Content-Type", "application/json")
                .header("Origin", "https://puter.com")
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (X11; Linux x86_64; rv:149.0) Gecko/20100101 Firefox/149.0",
                )
                .json(&envelope)
                .send()
                .await
                .map_err(|e| format!("Puter connection error: {}", e))?;

            let status = response.status().as_u16();

            if (status == 401 || status == 403) && auth_attempts < 1 {
                warn!("Puter auth error ({}), invalidating token...", status);
                let mut cached = self.cached_token.write().await;
                *cached = None;
                drop(cached);
                auth_attempts += 1;
                continue;
            }

            if status >= 400 {
                let body_text = response.text().await.unwrap_or_default();
                let trunc = &body_text[..body_text.len().min(500)];
                return Err(format!("Puter error {}: {}", status, trunc));
            }

            let resp_body: Value = response
                .json()
                .await
                .map_err(|e| format!("Puter response parse error: {}", e))?;

            // Convert Puter's `/drivers/call` non-streaming response → Anthropic MessagesResponse.
            // Expected shape: { "success": true, "result": { ...openai-completion-like... } }
            // We extract `result` if present; otherwise treat the whole body as the completion.
            let completion = resp_body.get("result").cloned().unwrap_or(resp_body);

            return Ok(convert_completion_to_anthropic(
                &completion,
                &request.model,
                input_tokens,
            ));
        }
    }
}

/// Convert an OpenAI-style chat completion JSON to an Anthropic MessagesResponse-compatible JSON.
fn convert_completion_to_anthropic(completion: &Value, model: &str, input_tokens: u32) -> Value {
    let message_id = format!("msg_{}", Uuid::new_v4());

    let choice = completion
        .get("choices")
        .and_then(|c| c.get(0))
        .cloned()
        .unwrap_or(json!({}));
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    let finish = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");

    let mut content_blocks: Vec<Value> = Vec::new();

    // Reasoning content
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !reasoning.is_empty() {
        content_blocks.push(json!({"type": "thinking", "thinking": reasoning}));
    }

    // Text content (may contain <think> tags or raw tool-call text — keep simple here)
    if let Some(text) = message.get("content").and_then(|v| v.as_str())
        && !text.is_empty()
    {
        content_blocks.push(json!({"type": "text", "text": text}));
    }

    // Native tool_calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args_str = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
            content_blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            }));
        }
    }

    if content_blocks.is_empty() {
        content_blocks.push(json!({"type": "text", "text": " "}));
    }

    let usage = completion.get("usage");
    let output_tokens = usage
        .and_then(|u| {
            u.get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    let stop_reason = map_stop_reason(Some(finish));

    json!({
        "id": message_id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content_blocks,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        }
    })
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

/// Truncate a string for logging — preserves UTF-8 boundaries and adds an ellipsis.
fn truncate_for_log(s: &str, max_chars: usize) -> String {
    let collected: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        format!("{}...({}+ chars)", collected, max_chars)
    } else {
        collected
    }
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
