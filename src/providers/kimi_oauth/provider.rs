use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::Stream;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::Settings;
use crate::converter::map_stop_reason;
use crate::models::anthropic::MessagesRequest;
use crate::models::openai::{ChatCompletionChunk, parse_sse_data_line};
use crate::rate_limiter::RateLimiter;
use crate::sse::SSEBuilder;

use super::auth::{AuthManager, build_common_headers};
use super::translate::build_kimi_request;

pub struct KimiOauthProvider {
    client: Client,
    auth: Arc<AuthManager>,
    rate_limiter: Arc<RateLimiter>,
    api_base_url: String,
}

impl KimiOauthProvider {
    pub fn new(settings: Settings, auth: Arc<AuthManager>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(settings.http_read_timeout))
            .connect_timeout(Duration::from_secs(settings.http_connect_timeout))
            .build()
            .expect("Failed to create HTTP client");

        let rate_limiter = RateLimiter::new(
            settings.provider_rate_limit,
            settings.provider_rate_window,
            settings.provider_max_concurrency,
        );

        Self {
            client,
            auth,
            rate_limiter,
            api_base_url: settings.provider_base_url("kimi_oauth"),
        }
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> impl Stream<Item = String> + use<> {
        let message_id = format!("msg_{}", Uuid::new_v4());
        let request_model = request.model.clone();
        let request_id = request_id.to_string();

        let mut body = build_kimi_request(request, Some(&request_id));
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".to_string(), serde_json::json!(true));
        }

        let url = format!(
            "{}/chat/completions",
            self.api_base_url.trim_end_matches('/')
        );
        let client = self.client.clone();
        let auth = self.auth.clone();
        let rate_limiter = self.rate_limiter.clone();

        info!("KIMI_OAUTH_STREAM: request_id={}", request_id);

        async_stream::stream! {
            let mut sse = SSEBuilder::new(message_id, request_model, input_tokens);
            yield sse.message_start();

            let _permit = rate_limiter.acquire_concurrency().await;

            let mut auth_attempts = 0;

            let last_error = 'auth_loop: loop {
                let token_str = match auth.get_auth().await {
                    Ok(a) => a.access,
                    Err(e) => break 'auth_loop format!("Auth failed: {}", e),
                };

                rate_limiter.acquire().await;
                let req = client.post(url.clone())
                    .headers(build_common_headers())
                    .header("Authorization", format!("Bearer {}", token_str))
                    .header("Accept", "text/event-stream")
                    .json(&body);

                let resp = req.send().await;

                match resp {
                    Err(e) => break 'auth_loop format!("Connection error: {}", e),
                    Ok(response) => {
                        let status = response.status().as_u16();

                        if status == 401 || status == 403 {
                            if auth_attempts < 1 {
                                warn!("Token rejected, forcing refresh...");
                                let _ = auth.force_refresh().await;
                                auth_attempts += 1;
                                continue 'auth_loop;
                            }
                            break 'auth_loop "Auth rejected repeatedly".to_string();
                        }

                        if status >= 400 {
                            let body_text = response.text().await.unwrap_or_default();
                            error!("Kimi API error {}: {}", status, body_text);
                            break 'auth_loop format!("API error {}: {}", status, body_text);
                        }

                        // Stream processing
                        let mut byte_stream = response.bytes_stream();
                        let mut line_buffer = String::new();
                        let mut finish_reason: Option<String> = None;
                        let mut usage_output_tokens: Option<u32> = None;
                        let mut had_tool_call = false;
                        let mut stream_read_error: Option<String> = None;

                        'stream: loop {
                            let stream_ended = match byte_stream.next().await {
                                Some(Ok(bytes)) => {
                                    line_buffer.push_str(&String::from_utf8_lossy(&bytes));
                                    false
                                }
                                Some(Err(e)) => {
                                    stream_read_error = Some(format!("Stream error: {}", e));
                                    break 'stream;
                                }
                                None => {
                                    if line_buffer.trim().is_empty() {
                                        break 'stream;
                                    }
                                    line_buffer.push('\n');
                                    true
                                }
                            };

                            while let Some(newline_pos) = line_buffer.find('\n') {
                                let line = line_buffer[..newline_pos].trim().to_string();
                                line_buffer = line_buffer[newline_pos + 1..].to_string();

                                if line.is_empty() {
                                    continue;
                                }

                                let Some(data) = parse_sse_data_line(&line) else {
                                    continue;
                                };
                                if data == "[DONE]" {
                                    break 'stream;
                                }

                                let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(_) => continue,
                                };

                                if let Some(ref usage) = chunk.usage
                                    && let Some(ct) = usage.completion_tokens {
                                        usage_output_tokens = Some(ct);
                                    }

                                if chunk.choices.is_empty() { continue; }
                                let choice = &chunk.choices[0];

                                if let Some(ref reason) = choice.finish_reason {
                                    finish_reason = Some(reason.clone());
                                }

                                if let Some(ref delta) = choice.delta {
                                    if let Some(ref reasoning) = delta.reasoning_content {
                                        for event in sse.ensure_thinking_block() { yield event; }
                                        yield sse.emit_thinking_delta(reasoning);
                                    }
                                    if let Some(ref content) = delta.content {
                                        for event in sse.ensure_text_block() { yield event; }
                                        yield sse.emit_text_delta(content);
                                    }
                                    if let Some(ref tool_calls) = delta.tool_calls {
                                        for event in sse.close_content_blocks() { yield event; }
                                        for tc in tool_calls {
                                            let tc_index = tc.index.unwrap_or(0);
                                            if let Some(ref id) = tc.id {
                                                sse.blocks.register_tool_name(tc_index, "");
                                                if let Some(state) = sse.blocks.tool_states.get_mut(&tc_index)
                                                    && state.tool_id.is_empty()
                                                {
                                                    state.tool_id = id.clone();
                                                }
                                            }
                                            if let Some(ref func) = tc.function
                                                && let Some(ref name) = func.name
                                                && !name.is_empty()
                                            {
                                                sse.blocks.register_tool_name(tc_index, name);
                                            }
                                            let state_started = sse.blocks.tool_states.get(&tc_index).is_some_and(|s| s.started);
                                            if !state_started {
                                                let name = sse.blocks.tool_states.get(&tc_index).map(|s| s.name.clone()).unwrap_or_default();
                                                if name.is_empty() {
                                                    if let Some(ref func) = tc.function
                                                        && let Some(ref args) = func.arguments
                                                        && !args.is_empty()
                                                    {
                                                        sse.blocks.register_tool_name(tc_index, "");
                                                        if let Some(state) = sse.blocks.tool_states.get_mut(&tc_index) {
                                                            state.contents.push(args.clone());
                                                        }
                                                    }
                                                    continue;
                                                }
                                                let buffered_args = sse.blocks.tool_states
                                                    .get(&tc_index)
                                                    .map(|s| s.contents.clone())
                                                    .unwrap_or_default();
                                                let tool_id = sse.blocks.tool_states
                                                    .get(&tc_index)
                                                    .and_then(|s| (!s.tool_id.is_empty()).then(|| s.tool_id.clone()))
                                                    .or_else(|| tc.id.clone())
                                                    .unwrap_or_else(|| format!("tool_{}", Uuid::new_v4()));
                                                had_tool_call = true;
                                                yield sse.start_tool_block(tc_index, &tool_id, &name);
                                                if !buffered_args.is_empty() && name != "Task" {
                                                    let block_idx = sse.blocks.tool_states
                                                        .get(&tc_index)
                                                        .map(|s| s.block_index as u32)
                                                        .unwrap_or(0);
                                                    for args in &buffered_args {
                                                        yield sse.content_block_delta(
                                                            block_idx,
                                                            "input_json_delta",
                                                            args,
                                                        );
                                                    }
                                                }
                                            }
                                            if let Some(ref func) = tc.function
                                                && let Some(ref args) = func.arguments
                                                    && !args.is_empty() {
                                                        let current_name = sse.blocks.tool_states
                                                            .get(&tc_index)
                                                            .map(|s| s.name.as_str())
                                                            .unwrap_or("");
                                                        if current_name == "Task" {
                                                            if let Some(state) = sse.blocks.tool_states.get_mut(&tc_index) {
                                                                state.contents.push(args.clone());
                                                            }
                                                        } else {
                                                        yield sse.emit_tool_delta(tc_index, args);
                                                        }
                                                    }
                                        }
                                    }
                                }
                            }
                            if stream_ended {
                                break 'stream;
                            }
                        }

                        if let Some(err) = stream_read_error {
                            break 'auth_loop err;
                        }

                        let has_started_content = sse.blocks.text_index >= 0
                            || sse.blocks.thinking_index >= 0
                            || sse.blocks.tool_states.values().any(|state| state.started);
                        if !had_tool_call && !has_started_content {
                            for event in sse.ensure_text_block() { yield event; }
                            yield sse.emit_text_delta(" ");
                        }

                        for event in sse.flush_task_tool_inputs() { yield event; }
                        for event in sse.close_all_blocks() { yield event; }

                        let output_tokens = usage_output_tokens.unwrap_or_else(|| sse.estimate_output_tokens());
                        let stop = if had_tool_call {
                            "tool_use"
                        } else {
                            map_stop_reason(finish_reason.as_deref())
                        };
                        yield sse.message_delta(stop, output_tokens);
                        yield sse.message_stop();
                        return;
                    }
                }
            };

            for event in sse.close_content_blocks() { yield event; }
            for event in sse.emit_error(&last_error) { yield event; }
            for event in sse.close_all_blocks() { yield event; }
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
        let mut body = build_kimi_request(request, Some(request_id));
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".to_string(), serde_json::json!(false));
        }

        let url = format!(
            "{}/chat/completions",
            self.api_base_url.trim_end_matches('/')
        );

        let mut auth_attempts = 0;
        loop {
            let token_str = self.auth.get_auth().await?.access;
            self.rate_limiter.acquire().await;

            let response = self
                .client
                .post(&url)
                .headers(build_common_headers())
                .header("Authorization", format!("Bearer {}", token_str))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Connection error: {}", e))?;

            let status = response.status().as_u16();
            if status == 401 || status == 403 {
                if auth_attempts < 1 {
                    let _ = self.auth.force_refresh().await;
                    auth_attempts += 1;
                    continue;
                }
                return Err("Auth rejected repeatedly".to_string());
            }

            if status >= 400 {
                let body_text = response.text().await.unwrap_or_default();
                return Err(format!("Provider error {}: {}", status, body_text));
            }

            let resp_body: Value = response
                .json()
                .await
                .map_err(|e| format!("Parse error: {}", e))?;

            // Reusing OpenAI Compat non-streaming fallback mapping
            let message_id = format!("msg_{}", Uuid::new_v4());
            let choice = resp_body
                .get("choices")
                .and_then(|c| c.get(0))
                .ok_or("No choices in response")?;
            let message = choice.get("message").ok_or("No message in choice")?;
            let finish = choice
                .get("finish_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("stop");

            let mut content_blocks: Vec<Value> = Vec::new();

            if let Some(text) = message.get("content").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                content_blocks.push(serde_json::json!({"type": "text", "text": text}));
            }

            let reasoning_val = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"));
            if let Some(reasoning) = reasoning_val.and_then(|v| v.as_str())
                && !reasoning.is_empty()
            {
                content_blocks.push(serde_json::json!({"type": "thinking", "thinking": reasoning}));
            }

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
                    let mut input: Value =
                        serde_json::from_str(args_str).unwrap_or_else(|_| serde_json::json!({}));
                    if name == "Task"
                        && let Some(obj) = input.as_object_mut()
                    {
                        obj.insert("run_in_background".to_string(), serde_json::json!(false));
                    }
                    content_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }
            }

            let emitted_tool = content_blocks
                .iter()
                .any(|block| block.get("type").and_then(|v| v.as_str()) == Some("tool_use"));

            if content_blocks.is_empty() {
                content_blocks.push(serde_json::json!({"type": "text", "text": " "}));
            }

            let usage = resp_body.get("usage");
            let output_tokens = usage
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32;
            let stop_reason = if emitted_tool {
                "tool_use"
            } else {
                map_stop_reason(Some(finish))
            };

            return Ok(serde_json::json!({
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": request.model,
                "content": content_blocks,
                "stop_reason": stop_reason,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }));
        }
    }
}
