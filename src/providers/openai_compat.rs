use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::Stream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::{Settings, get_provider_api_key, get_provider_base_url};
use crate::converter::{build_openai_request, map_stop_reason};
use crate::heuristic_tool_parser::HeuristicToolParser;
use crate::ip_rotator;
use crate::models::anthropic::MessagesRequest;
use crate::models::openai::ChatCompletionChunk;
use crate::rate_limiter::RateLimiter;
use crate::sse::SSEBuilder;
use crate::think_parser::{ContentType, ThinkTagParser};

pub struct OpenAICompatProvider {
    client: Client,
    rate_limiter: Arc<RateLimiter>,
    enable_ip_rotation: bool,
}

impl OpenAICompatProvider {
    pub fn new(settings: &Settings) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(settings.http_read_timeout))
            .connect_timeout(Duration::from_secs(settings.http_connect_timeout))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let rate_limiter = RateLimiter::new(
            settings.provider_rate_limit,
            settings.provider_rate_window,
            settings.provider_max_concurrency,
        );

        Self {
            client,
            rate_limiter,
            enable_ip_rotation: settings.enable_ip_rotation,
        }
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> impl Stream<Item = String> + use<> {
        let resolved = request
            .resolved_provider_model
            .clone()
            .unwrap_or_else(|| request.model.clone());

        let provider_type = Settings::parse_provider_type(&resolved).to_string();
        let model_name = Settings::parse_model_name(&resolved).to_string();
        let base_url = get_provider_base_url(&provider_type);
        let api_key = get_provider_api_key(&provider_type).unwrap_or_default();
        let request_model = request.model.clone();

        let message_id = format!("msg_{}", Uuid::new_v4());
        let body = build_openai_request(request, &model_name);
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let request_id = request_id.to_string();
        let rate_limiter = self.rate_limiter.clone();
        let client = self.client.clone();
        let enable_ip_rotation = self.enable_ip_rotation;

        info!(
            "STREAM: request_id={} provider={} model={} msgs={} tools={}",
            request_id,
            provider_type,
            model_name,
            body.messages.len(),
            body.tools.as_ref().map_or(0, |t| t.len()),
        );

        async_stream::stream! {
            let mut sse = SSEBuilder::new(message_id, request_model, input_tokens);
            yield sse.message_start();

            let _permit = rate_limiter.acquire_concurrency().await;

            let max_retries: u32 = 3;
            let mut last_error: Option<String> = None;

            for attempt in 0..=max_retries {
                rate_limiter.acquire().await;

                let resp = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Accept", "text/event-stream")
                    .json(&body)
                    .send()
                    .await;

                match resp {
                    Err(e) => {
                        error!("STREAM_ERROR: request_id={} attempt={} error={}", request_id, attempt, e);
                        last_error = Some(format!("Connection error: {}", e));
                        if attempt < max_retries {
                            let delay = (2u64.pow(attempt)) as f64 + rand_jitter();
                            warn!("Retrying in {:.1}s (attempt {}/{})", delay, attempt + 1, max_retries);
                            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                            continue;
                        }
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();

                        if status == 429 {
                            warn!("Rate limited (429): request_id={} attempt={}", request_id, attempt);

                            if attempt < max_retries {
                                let delay = (2u64.pow(attempt) * 2) as f64 + rand_jitter();
                                warn!("Retrying in {:.1}s (attempt {}/{})", delay, attempt + 1, max_retries);
                                rate_limiter.set_blocked(delay).await;
                                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                continue;
                            }

                            warn!("All retries exhausted. Setting strict block and initiating IP rotation...");
                            rate_limiter.set_blocked(60.0).await;

                            if enable_ip_rotation {
                                let rl = rate_limiter.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = ip_rotator::rotate_ip().await {
                                        error!("IP rotation failed: {}", e);
                                    } else {
                                        rl.clear_block().await;
                                    }
                                });
                            }

                            last_error = Some("Rate limit exceeded. Retries exhausted.".to_string());
                            break;
                        }

                        if status >= 400 {
                            let body_text = response.text().await.unwrap_or_default();
                            error!("Provider error {}: {}", status, body_text);

                            // Extract readable error message from provider response
                            let provider_msg = extract_provider_error(&body_text);
                            last_error = Some(format!(
                                "Provider returned status {} (request_id={}): {}",
                                status, request_id, provider_msg
                            ));

                            if status >= 500 && attempt < max_retries {
                                let delay = (2u64.pow(attempt)) as f64 + rand_jitter();
                                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                continue;
                            }
                            break;
                        }

                        // --- Successful response: process stream ---
                        let mut think_parser = ThinkTagParser::new();
                        let mut heuristic_parser = HeuristicToolParser::new();
                        let mut finish_reason: Option<String> = None;
                        let mut usage_output_tokens: Option<u32> = None;
                        let mut byte_stream = response.bytes_stream();
                        let mut line_buffer = String::new();

                        while let Some(chunk_result) = byte_stream.next().await {
                            let bytes = match chunk_result {
                                Ok(b) => b,
                                Err(e) => {
                                    error!("Stream read error: {}", e);
                                    last_error = Some(format!("error decoding response body: {}", e));
                                    break;
                                }
                            };

                            line_buffer.push_str(&String::from_utf8_lossy(&bytes));

                            while let Some(newline_pos) = line_buffer.find('\n') {
                                let line = line_buffer[..newline_pos].trim().to_string();
                                line_buffer = line_buffer[newline_pos + 1..].to_string();

                                if line.is_empty() || !line.starts_with("data: ") {
                                    continue;
                                }

                                let data = &line[6..];
                                if data == "[DONE]" {
                                    break;
                                }

                                let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        debug!("Skipping unparseable chunk: {} ({})", data, e);
                                        continue;
                                    }
                                };

                                if let Some(ref usage) = chunk.usage
                                    && let Some(ct) = usage.completion_tokens {
                                        usage_output_tokens = Some(ct);
                                    }

                                if chunk.choices.is_empty() {
                                    continue;
                                }

                                let choice = &chunk.choices[0];

                                if let Some(ref reason) = choice.finish_reason {
                                    finish_reason = Some(reason.clone());
                                }

                                if let Some(ref delta) = choice.delta {
                                    // Handle native reasoning_content (DeepSeek, etc.)
                                    if let Some(ref reasoning) = delta.reasoning_content {
                                        for event in sse.ensure_thinking_block() {
                                            yield event;
                                        }
                                        yield sse.emit_thinking_delta(reasoning);
                                    }

                                    // Handle text content with think-tag and heuristic tool parsing
                                    if let Some(ref content) = delta.content {
                                        let think_chunks = think_parser.feed(content);
                                        for c in think_chunks {
                                            match c.content_type {
                                                ContentType::Thinking => {
                                                    for event in sse.ensure_thinking_block() {
                                                        yield event;
                                                    }
                                                    yield sse.emit_thinking_delta(&c.content);
                                                }
                                                ContentType::Text => {
                                                    // Run heuristic tool parser on text content
                                                    let (filtered_text, detected_tools) =
                                                        heuristic_parser.feed(&c.content);

                                                    if !filtered_text.is_empty() {
                                                        for event in sse.ensure_text_block() {
                                                            yield event;
                                                        }
                                                        yield sse.emit_text_delta(&filtered_text);
                                                    }

                                                    // Emit detected heuristic tool calls
                                                    for tool_use in &detected_tools {
                                                        for event in sse.close_content_blocks() {
                                                            yield event;
                                                        }
                                                        let block_idx = sse.blocks.allocate_index();
                                                        let mut input = tool_use.input.clone();
                                                        // Force Task subagent to foreground
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

                                    // Handle native tool calls from provider
                                    if let Some(ref tool_calls) = delta.tool_calls {
                                        for event in sse.close_content_blocks() {
                                            yield event;
                                        }
                                        for tc in tool_calls {
                                            let tc_index = tc.index.unwrap_or(0);

                                            if let Some(ref func) = tc.function
                                                && let Some(ref name) = func.name {
                                                    sse.blocks.register_tool_name(tc_index, name);
                                                }

                                            let state_started = sse.blocks.tool_states
                                                .get(&tc_index)
                                                .is_some_and(|s| s.started);

                                            if !state_started {
                                                let tool_id = tc.id.clone().unwrap_or_else(|| format!("tool_{}", Uuid::new_v4()));
                                                let name = sse.blocks.tool_states.get(&tc_index)
                                                    .map(|s| s.name.clone())
                                                    .unwrap_or_default();

                                                if name.is_empty() && tc.id.is_none()
                                                    && let Some(ref func) = tc.function
                                                    && func.arguments.as_ref().is_some_and(|a| !a.is_empty())
                                                {
                                                    // Buffer args; don't start block until we have a name
                                                    if let Some(ref args) = func.arguments {
                                                        sse.blocks.register_tool_name(tc_index, "");
                                                        if let Some(state) = sse.blocks.tool_states.get_mut(&tc_index) {
                                                            state.contents.push(args.clone());
                                                        }
                                                    }
                                                    continue;
                                                }

                                                yield sse.start_tool_block(tc_index, &tool_id, &name);
                                            }

                                            if let Some(ref func) = tc.function
                                                && let Some(ref args) = func.arguments
                                                    && !args.is_empty() {
                                                        // Buffer Task tool args to force run_in_background: false
                                                        let current_name = sse.blocks.tool_states
                                                            .get(&tc_index)
                                                            .map(|s| s.name.as_str())
                                                            .unwrap_or("");

                                                        if current_name == "Task" {
                                                            let state = sse.blocks.tool_states.get_mut(&tc_index);
                                                            if let Some(state) = state {
                                                                state.contents.push(args.clone());
                                                                // Try parsing the accumulated JSON
                                                                let accumulated: String = state.contents.iter().cloned().collect();
                                                                if let Ok(mut parsed) = serde_json::from_str::<Value>(&accumulated) {
                                                                    if let Some(obj) = parsed.as_object_mut() {
                                                                        obj.insert("run_in_background".to_string(), json!(false));
                                                                    }
                                                                    let patched = serde_json::to_string(&parsed).unwrap_or_default();
                                                                    // Emit the full patched JSON as a single delta
                                                                    let block_idx = state.block_index;
                                                                    yield sse.content_block_delta(
                                                                        block_idx as u32,
                                                                        "input_json_delta",
                                                                        &patched,
                                                                    );
                                                                }
                                                            }
                                                        } else {
                                                            yield sse.emit_tool_delta(tc_index, args);
                                                        }
                                                    }
                                        }
                                    }
                                }
                            }
                        }

                        if last_error.is_some() {
                            break;
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
                                    for event in sse.ensure_text_block() {
                                        yield event;
                                    }
                                    yield sse.emit_text_delta(&remaining.content);
                                }
                            }
                        }

                        // Flush heuristic tool parser
                        for tool_use in heuristic_parser.flush() {
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
                        let stop = map_stop_reason(finish_reason.as_deref());
                        yield sse.message_delta(stop, output_tokens);
                        yield sse.message_stop();
                        return;
                    }
                }
            }

            // Error path: emit error to Claude Code
            let error_msg = last_error.unwrap_or_else(|| "Unknown error".to_string());
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
}

/// Extract a human-readable error message from the provider's JSON error body.
fn extract_provider_error(body: &str) -> String {
    // Try parsing as JSON to extract the message
    if let Ok(val) = serde_json::from_str::<Value>(body) {
        // Standard: {"error": {"message": "..."}}
        if let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
        // Array: [{"error": {"message": "..."}}]
        if let Some(arr) = val.as_array() {
            for item in arr {
                if let Some(msg) = item
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return msg.to_string();
                }
            }
        }
        // Fallback: {"message": "..."}
        if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    // Not JSON, return truncated raw body
    if body.len() > 200 {
        format!("{}...", &body[..200])
    } else {
        body.to_string()
    }
}

fn rand_jitter() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}
