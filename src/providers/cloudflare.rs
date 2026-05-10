use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::Stream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::key_pool::KeyPoolManager;

use crate::config::{Settings, get_provider_api_key, get_provider_base_url};
use crate::converter::{build_openai_request, map_stop_reason};
use crate::heuristic_tool_parser::HeuristicToolParser;
use crate::models::anthropic::MessagesRequest;
use crate::models::openai::{ChatCompletionChunk, ChatMessage};
use crate::rate_limiter::RateLimiter;
use crate::sse::SSEBuilder;
use crate::think_parser::{ContentType, ThinkTagParser};
use crate::tool_intent_detector;

pub struct CloudflareProvider {
    client: Client,
    rate_limiter: Arc<RateLimiter>,
    enable_ip_rotation: bool,
    enable_tool_retry: bool,
    tool_retry_max: u32,
}

impl CloudflareProvider {
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
            enable_tool_retry: settings.enable_tool_retry,
            tool_retry_max: settings.tool_retry_max,
        }
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
        key_pool: Arc<KeyPoolManager>,
        model_router: Arc<crate::model_router::ModelRouter>,
    ) -> impl Stream<Item = String> + use<> {
        let mut resolved_spec = request
            .resolved_provider_model
            .clone()
            .unwrap_or_else(|| request.model.clone());

        let request_model = request
            .original_model
            .clone()
            .unwrap_or_else(|| request.model.clone());
        let original_request = request.clone(); // Keep full request for body rebuilds on fallback
        let request_id = request_id.to_string();
        let rate_limiter = self.rate_limiter.clone();
        let client = self.client.clone();
        let _enable_ip_rotation = self.enable_ip_rotation;
        let enable_tool_retry = self.enable_tool_retry;
        let tool_retry_max = self.tool_retry_max;

        let initial_provider = Settings::parse_provider_type(&resolved_spec).to_string();
        let initial_model = Settings::parse_model_name(&resolved_spec).to_string();
        let mut current_body = build_openai_request(request, &initial_model, &initial_provider);
        let message_id = format!("msg_{}", Uuid::new_v4());

        async_stream::stream! {
            let mut sse = SSEBuilder::new(message_id, request_model.clone(), input_tokens);
            yield sse.message_start();

            let _permit = rate_limiter.acquire_concurrency().await;

            let mut last_error: Option<String> = None;

            // ── DRY retry loop for tool intent recovery ──────────────────
            let max_tool_attempts: u32 = if enable_tool_retry { tool_retry_max + 1 } else { 1 };
            let mut tool_attempt: u32 = 0;
            let mut accumulated_all_text = String::new();
            let mut had_tool_call = false;
            #[allow(unused_assignments)]
            let mut final_finish_reason: Option<String> = None;
            #[allow(unused_assignments)]
            let mut final_output_tokens: Option<u32> = None;

            'tool_retry: while tool_attempt < max_tool_attempts {
                let provider_type = Settings::parse_provider_type(&resolved_spec).to_string();
                let model_name = Settings::parse_model_name(&resolved_spec).to_string();
                let base_url = get_provider_base_url(&provider_type);

                // ── 3-tier escalation: key rotation → model fallback → IP rotation ──
                'key_rotation: loop {
                    let key_ep = key_pool.acquire(&provider_type).await;
                    let api_key_full = key_ep.as_ref()
                        .map(|ep| ep.key.clone())
                        .unwrap_or_else(|| get_provider_api_key(&provider_type).unwrap_or_default());

                    let parts: Vec<&str> = api_key_full.split(':').collect();
                    let account_id = parts.first().unwrap_or(&"").to_string();
                    let auth_token = parts.get(1).unwrap_or(&"").to_string();
                    let key_preview = crate::key_pool::mask_key(&api_key_full);

                    if key_ep.is_none() && key_pool.all_exhausted(&provider_type).await {
                        // All keys for this provider are on cooldown → escalate to model fallback
                        warn!(
                            "ALL_KEYS_EXHAUSTED: provider={} → escalating to model fallback",
                            provider_type
                        );
                        break 'key_rotation;
                    }

                    let url = format!(
                        "{}/{}/ai/run/{}",
                        base_url.trim_end_matches('/'),
                        account_id,
                        model_name
                    );

                    info!(
                        "CF_STREAM_ATTEMPT: request_id={} provider={} model={} key={} msgs={} tools={}",
                        request_id, provider_type, model_name, key_preview,
                        current_body.messages.len(), current_body.tools.as_ref().map_or(0, |t| t.len())
                    );

                    // Level 1: same-key retry for 5xx/timeout only
                    let max_5xx_retries: u32 = 2;
                    let mut five_xx_attempt: u32 = 0;

                    loop {
                        rate_limiter.acquire().await;

                        let req = client
                            .post(&url)
                            .header("Content-Type", "application/json")
                            .header("Authorization", format!("Bearer {}", auth_token))
                            .header("Accept", "text/event-stream")
                            .json(&current_body);

                        let resp = req.send().await;

                        match resp {
                            Err(e) => {
                                if let Some(ep) = &key_ep {
                                    key_pool.report_error(ep, false).await;
                                }
                                error!("CF_STREAM_ERROR: request_id={} error={}", request_id, e);
                                last_error = Some(format!("Connection error: {}", e));
                                if five_xx_attempt < max_5xx_retries {
                                    five_xx_attempt += 1;
                                    let delay = (2u64.pow(five_xx_attempt - 1)) as f64 + rand_jitter();
                                    warn!("Retrying same key in {:.1}s ({}/{})", delay, five_xx_attempt, max_5xx_retries);
                                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                    continue;
                                }
                                continue 'key_rotation;
                            }
                            Ok(response) => {
                                let status = response.status().as_u16();

                                if status == 429 {
                                    if let Some(ep) = &key_ep {
                                        key_pool.report_error(ep, true).await;
                                    }
                                    warn!(
                                        "CF_RATE_LIMITED: request_id={} provider={} key={} → rotating key",
                                        request_id, provider_type, key_preview
                                    );
                                    last_error = Some(format!("Rate limit (429) on key {}", key_preview));
                                    continue 'key_rotation;
                                }

                                if status >= 400 {
                                    let body_text = response.text().await.unwrap_or_default();
                                    error!("Provider error {}: {}", status, body_text);

                                    if status == 500 && body_text.contains("unhashable") {
                                        error!("Captured unhashable-error payload → failed_payload.json");
                                        std::fs::write(
                                            "failed_payload.json",
                                            serde_json::to_string_pretty(&current_body).unwrap_or_default(),
                                        ).ok();
                                    }

                                    let provider_msg = extract_provider_error(&body_text);
                                    last_error = Some(format!(
                                        "Provider returned status {} (request_id={}): {}",
                                        status, request_id, provider_msg
                                    ));

                                    if status >= 500 {
                                        if let Some(ep) = &key_ep {
                                            key_pool.report_error(ep, false).await;
                                        }
                                        if five_xx_attempt < max_5xx_retries {
                                            five_xx_attempt += 1;
                                            let delay = (2u64.pow(five_xx_attempt - 1)) as f64 + rand_jitter();
                                            warn!("5xx retry same key in {:.1}s ({}/{})", delay, five_xx_attempt, max_5xx_retries);
                                            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                            continue;
                                        }
                                        continue 'key_rotation;
                                    }
                                    break 'key_rotation;
                                }

                                if let Some(ep) = &key_ep {
                                    // Report success (approximated latency, can be enhanced)
                                    key_pool.report_success(ep, 100).await;
                                }
                                info!(
                                    "CF_STREAM_OK: request_id={} provider={} model={} key={}",
                                    request_id, provider_type, model_name, key_preview
                                );

                                // ── Successful response: process stream ──────────
                            let mut think_parser = ThinkTagParser::new();
                            let mut heuristic_parser = HeuristicToolParser::new();
                            let mut finish_reason: Option<String> = None;
                            let mut usage_output_tokens: Option<u32> = None;
                            let mut byte_stream = response.bytes_stream();
                            let mut line_buffer = String::new();
                            let mut attempt_text = String::new();
                            let mut attempt_had_tool = false;

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
                                            attempt_text.push_str(content);
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
                                                            attempt_had_tool = true;
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
                                                attempt_had_tool = true;
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
                                                                    let mut patched = String::new();
                                                                    if let Ok(mut parsed) = serde_json::from_str::<Value>(&accumulated) {
                                                                        if let Some(obj) = parsed.as_object_mut() {
                                                                            obj.insert("run_in_background".to_string(), json!(false));
                                                                        }
                                                                        patched = serde_json::to_string(&parsed).unwrap_or_default();
                                                                    } else {
                                                                        let garbled = format!(r#"{{"name": "Task", "arguments": {}}}"#, accumulated);
                                                                        if let Some(recovered) = crate::heuristic_tool_parser::recover_garbled_tool_json(&garbled) {
                                                                            let mut parsed = serde_json::Value::Object(recovered.arguments);
                                                                            if let Some(obj) = parsed.as_object_mut() {
                                                                                obj.insert("run_in_background".to_string(), json!(false));
                                                                            }
                                                                            patched = serde_json::to_string(&parsed).unwrap_or_default();
                                                                        }
                                                                    }

                                                                    if !patched.is_empty() {
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
                                break 'tool_retry;
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
                                        for event in sse.ensure_text_block() {
                                            yield event;
                                        }
                                        yield sse.emit_text_delta(&remaining.content);
                                    }
                                }
                            }

                            // Flush heuristic tool parser
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
                            final_finish_reason = finish_reason;
                            final_output_tokens = usage_output_tokens;

                            // ── POST-STREAM: Retry decision ──────────────
                            // Only retry if:
                            // 1. No tool calls were detected (native or heuristic)
                            // 2. Tool intent was detected in the text
                            // 3. We haven't exhausted tool retry attempts
                            if !had_tool_call
                                && enable_tool_retry
                                && tool_attempt + 1 < max_tool_attempts
                                && tool_intent_detector::has_tool_intent(&accumulated_all_text)
                            {
                                info!(
                                    "TOOL_RETRY: request_id={} attempt={}/{} — Tool intent detected but no tool call produced. Retrying...",
                                    request_id, tool_attempt + 1, max_tool_attempts - 1
                                );

                                // Close the current text block (we'll continue streaming from retry)
                                for event in sse.close_content_blocks() {
                                    yield event;
                                }

                                // Build retry body: append assistant text + corrective user message
                                current_body.messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: Some(attempt_text),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                current_body.messages.push(ChatMessage {
                                    role: "user".to_string(),
                                    content: Some(tool_intent_detector::RETRY_PROMPT.to_string()),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });

                                tool_attempt += 1;
                                continue 'tool_retry;
                            }

                            // ── No retry needed: finalize response ───────
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
                            let stop = map_stop_reason(final_finish_reason.as_deref());
                            yield sse.message_delta(stop, output_tokens);
                            yield sse.message_stop();
                            return;
                                } // end Ok(response)
                            } // end match resp
                        } // end inner 5xx loop
                    } // end 'key_rotation

                // ── Model fallback: all keys for current provider exhausted ──
                model_router.report_error_by_spec(&resolved_spec).await;
                if let Some(next_ep) = model_router.next_fallback(&request_model, &resolved_spec).await {
                    warn!(
                        "MODEL_FALLBACK: {} → {} (all keys for {} exhausted)",
                        resolved_spec, next_ep.full_spec,
                        Settings::parse_provider_type(&resolved_spec),
                    );
                    resolved_spec = next_ep.full_spec.clone();
                    // Fully rebuild the request body for the new provider type.
                    // This ensures correct schema sanitization and stream_options.
                    current_body = build_openai_request(
                        &original_request,
                        &next_ep.model_name,
                        Settings::parse_provider_type(&next_ep.full_spec),
                    );
                    continue 'tool_retry;
                }

                // If ALL fallback models are also exhausted, trigger IP rotation if enabled
                if _enable_ip_rotation {
                    warn!(
                        "ALL_MODELS_EXHAUSTED: request_id={} → Triggering WARP IP Rotation as last resort!",
                        request_id
                    );
                    if let Err(e) = crate::ip_rotator::rotate_ip().await {
                        error!("IP Rotation failed: {}", e);
                    } else {
                        info!("IP Rotation requested successfully");
                    }
                }

                // If we reached here from error path, break the tool retry loop
                break 'tool_retry;
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

    /// Send a non-streaming request to the provider and return an Anthropic MessagesResponse.
    ///
    /// **Fallback**: If `stream == Some(false)` in the request, this is used instead of SSE.
    /// Includes retry logic for 429s, 5xx, and connection errors.
    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
        key_pool: Arc<KeyPoolManager>,
        model_router: Arc<crate::model_router::ModelRouter>,
    ) -> Result<Value, String> {
        let request_model = request
            .original_model
            .clone()
            .unwrap_or_else(|| request.model.clone());
        let mut resolved_spec = request
            .resolved_provider_model
            .clone()
            .unwrap_or_else(|| request.model.clone());

        let _permit = self.rate_limiter.acquire_concurrency().await;
        let mut last_error = String::new();

        // Model fallback loop
        'model_fallback: loop {
            let provider_type = Settings::parse_provider_type(&resolved_spec).to_string();
            let model_name = Settings::parse_model_name(&resolved_spec).to_string();
            let base_url = get_provider_base_url(&provider_type);

            let mut body = build_openai_request(request, &model_name, &provider_type);
            body.stream = false;
            body.stream_options = None;

            // ── Key rotation loop ──
            'key_rotation: loop {
                let key_ep = key_pool.acquire(&provider_type).await;
                let api_key_full = key_ep
                    .as_ref()
                    .map(|ep| ep.key.clone())
                    .unwrap_or_else(|| get_provider_api_key(&provider_type).unwrap_or_default());

                let parts: Vec<&str> = api_key_full.split(':').collect();
                let account_id = parts.first().unwrap_or(&"").to_string();
                let auth_token = parts.get(1).unwrap_or(&"").to_string();
                let key_preview = crate::key_pool::mask_key(&api_key_full);

                if key_ep.is_none() && key_pool.all_exhausted(&provider_type).await {
                    warn!(
                        "ALL_KEYS_EXHAUSTED: provider={} → escalating to model fallback",
                        provider_type
                    );
                    break 'key_rotation;
                }

                let url = format!(
                    "{}/{}/ai/run/{}",
                    base_url.trim_end_matches('/'),
                    account_id,
                    model_name
                );

                info!(
                    "CF_NON_STREAM_ATTEMPT: request_id={} provider={} model={} key={}",
                    request_id, provider_type, model_name, key_preview
                );

                // Level 1: same-key retry for 5xx/timeout only
                let max_5xx_retries: u32 = 2;
                let mut five_xx_attempt: u32 = 0;

                loop {
                    self.rate_limiter.acquire().await;

                    let resp = self
                        .client
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .header("Authorization", format!("Bearer {}", auth_token))
                        .json(&body)
                        .send()
                        .await;

                    match resp {
                        Err(e) => {
                            if let Some(ep) = &key_ep {
                                key_pool.report_error(ep, false).await;
                            }
                            last_error = format!("Connection error: {}", e);
                            if five_xx_attempt < max_5xx_retries {
                                five_xx_attempt += 1;
                                let delay = (2u64.pow(five_xx_attempt - 1)) as f64 + rand_jitter();
                                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                continue;
                            }
                            continue 'key_rotation;
                        }
                        Ok(response) => {
                            let status = response.status().as_u16();

                            if status == 429 {
                                if let Some(ep) = &key_ep {
                                    key_pool.report_error(ep, true).await;
                                }
                                warn!(
                                    "CF_RATE_LIMITED: request_id={} provider={} key={} → rotating key",
                                    request_id, provider_type, key_preview
                                );
                                last_error = format!("Rate limit (429) on key {}", key_preview);
                                continue 'key_rotation;
                            }

                            if status >= 400 {
                                let body_text = response.text().await.unwrap_or_default();
                                let msg = extract_provider_error(&body_text);
                                last_error =
                                    format!("Provider returned status {}: {}", status, msg);
                                if status >= 500 {
                                    if let Some(ep) = &key_ep {
                                        key_pool.report_error(ep, false).await;
                                    }
                                    if five_xx_attempt < max_5xx_retries {
                                        five_xx_attempt += 1;
                                        let delay =
                                            (2u64.pow(five_xx_attempt - 1)) as f64 + rand_jitter();
                                        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                                        continue;
                                    }
                                    continue 'key_rotation;
                                }
                                break 'key_rotation;
                            }

                            if let Some(ep) = &key_ep {
                                key_pool.report_success(ep, 100).await;
                            }

                            // ── Success: parse response ──
                            let resp_body: Value = response
                                .json()
                                .await
                                .map_err(|e| format!("Failed to parse response: {}", e))?;

                            // Convert OpenAI ChatCompletion → Anthropic MessagesResponse
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
                                content_blocks.push(json!({"type": "text", "text": text}));
                            }

                            let reasoning_val = message
                                .get("reasoning_content")
                                .or_else(|| message.get("reasoning"));
                            if let Some(reasoning) = reasoning_val.and_then(|v| v.as_str())
                                && !reasoning.is_empty()
                            {
                                content_blocks
                                    .push(json!({"type": "thinking", "thinking": reasoning}));
                            }

                            if let Some(tool_calls) =
                                message.get("tool_calls").and_then(|v| v.as_array())
                            {
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
                                    let input: Value = match serde_json::from_str(args_str) {
                                        Ok(val) => val,
                                        Err(_) => {
                                            let garbled = format!(
                                                r#"{{"name": "{}", "arguments": {}}}"#,
                                                name, args_str
                                            );
                                            crate::heuristic_tool_parser::recover_garbled_tool_json(
                                                &garbled,
                                            )
                                            .map(|rec| serde_json::Value::Object(rec.arguments))
                                            .unwrap_or_else(|| json!({}))
                                        }
                                    };
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

                            let usage = resp_body.get("usage");
                            let output_tokens = usage
                                .and_then(|u| u.get("completion_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(1)
                                as u32;

                            let stop_reason = map_stop_reason(Some(finish));

                            return Ok(json!({
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
            }

            // ── Model fallback: all keys for current provider exhausted ──
            model_router.report_error_by_spec(&resolved_spec).await;
            if let Some(next_ep) = model_router
                .next_fallback(&request_model, &resolved_spec)
                .await
            {
                warn!(
                    "MODEL_FALLBACK: {} → {} (all keys for {} exhausted)",
                    resolved_spec,
                    next_ep.full_spec,
                    Settings::parse_provider_type(&resolved_spec),
                );
                resolved_spec = next_ep.full_spec.clone();
                continue 'model_fallback;
            }

            // If ALL fallback models are also exhausted, trigger IP rotation if enabled
            if self.enable_ip_rotation {
                warn!(
                    "ALL_MODELS_EXHAUSTED: request_id={} → Triggering WARP IP Rotation as last resort!",
                    request_id
                );
                if let Err(e) = crate::ip_rotator::rotate_ip().await {
                    error!("IP Rotation failed: {}", e);
                } else {
                    info!("IP Rotation requested successfully");
                }
            }

            return Err(last_error);
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
    // Mix in thread identity for decorrelation across concurrent calls
    let tid = format!("{:?}", std::thread::current().id());
    let tid_hash: u32 = tid
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    (nanos.wrapping_add(tid_hash) % 1000) as f64 / 1000.0
}
