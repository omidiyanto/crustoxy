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
use crate::converter::build_openai_request;
use crate::models::anthropic::MessagesRequest;
use crate::sse::SSEBuilder;
use crate::think_parser::{ContentType, ThinkTagParser};

const LOGIN_URL: &str = "https://puter.com/login";
const CHAT_URL: &str = "https://api.puter.com/drivers/call";

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
}

impl PuterProvider {
    pub async fn new(credentials: &str, settings: &Settings) -> Result<Self, String> {
        let (username, password) = credentials
            .split_once(':')
            .ok_or_else(|| "PUTER_API_KEY must be in format 'username:password'".to_string())?;

        let client = Client::builder()
            .timeout(Duration::from_secs(settings.http_read_timeout))
            .connect_timeout(Duration::from_secs(settings.http_connect_timeout))
            .pool_max_idle_per_host(5)
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let provider = Self {
            client,
            username: username.to_string(),
            password: password.to_string(),
            cached_token: Arc::new(RwLock::new(None)),
        };

        // Lazy login: defer to first request so WARP/network has time to initialize
        info!("Puter provider created (login deferred to first request)");
        Ok(provider)
    }

    /// Get a valid token, re-authenticating only if needed.
    async fn ensure_token(&self) -> Result<String, String> {
        // Fast path: check if we have a valid cached token
        {
            let cached = self.cached_token.read().await;
            if let Some(ref ct) = *cached {
                if ct.obtained_at.elapsed().as_secs() < TOKEN_LIFETIME_SECS {
                    return Ok(ct.token.clone());
                }
                info!("Puter token expired, re-authenticating...");
            }
        }

        // Slow path: need to login
        let token = self.login().await?;

        // Store the token
        {
            let mut cached = self.cached_token.write().await;
            *cached = Some(CachedToken {
                token: token.clone(),
                obtained_at: Instant::now(),
            });
        }

        Ok(token)
    }

    /// Perform login to Puter with retry and exponential backoff.
    async fn login(&self) -> Result<String, String> {
        let mut last_err = String::new();

        for attempt in 1..=LOGIN_MAX_RETRIES {
            info!(
                "Puter: login attempt {}/{} as '{}'...",
                attempt, LOGIN_MAX_RETRIES, self.username
            );

            let payload = json!({
                "username": self.username,
                "password": self.password,
            });

            let result = self
                .client
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
                        // Don't retry on 401/403 (bad credentials)
                        if status == 401 || status == 403 {
                            return Err(last_err);
                        }
                        warn!(
                            "Puter login attempt {}/{} failed: {}",
                            attempt, LOGIN_MAX_RETRIES, last_err
                        );
                    } else {
                        let token =
                            body.get("token").and_then(|v| v.as_str()).ok_or_else(|| {
                                "Puter login response missing 'token' field".to_string()
                            })?;
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

    /// Invalidate the cached token so next call re-authenticates.
    async fn invalidate_token(&self) {
        let mut cached = self.cached_token.write().await;
        *cached = None;
    }

    /// Build the Puter chat payload from a list of OpenAI-style messages.
    fn build_chat_payload(token: &str, model: &str, messages: &[Value], stream: bool) -> Value {
        json!({
            "interface": "puter-chat-completion",
            "driver": "ai-chat",
            "test_mode": false,
            "method": "complete",
            "args": {
                "messages": messages,
                "model": model,
                "stream": stream,
            },
            "auth_token": token,
        })
    }

    /// Convert our internal OpenAI messages format to Puter's simplified messages format.
    fn convert_messages_for_puter(request: &MessagesRequest) -> Vec<Value> {
        let openai_req = build_openai_request(request, &request.model);
        let mut messages = Vec::new();

        for msg in &openai_req.messages {
            let mut puter_msg = json!({});

            if let Some(ref content) = msg.content {
                puter_msg["content"] = json!(content);
            }

            // Map roles
            match msg.role.as_str() {
                "system" => puter_msg["role"] = json!("system"),
                "assistant" => puter_msg["role"] = json!("assistant"),
                "tool" => {
                    // Puter doesn't support tool role; convert to user
                    puter_msg["role"] = json!("user");
                    if let Some(ref content) = msg.content {
                        puter_msg["content"] = json!(format!("[Tool Result]: {}", content));
                    }
                }
                _ => puter_msg["role"] = json!("user"),
            }

            messages.push(puter_msg);
        }

        messages
    }

    pub fn stream_response(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> impl Stream<Item = String> + use<> {
        let model_name = Settings::parse_model_name(
            request
                .resolved_provider_model
                .as_deref()
                .unwrap_or(&request.model),
        )
        .to_string();

        let request_model = request.model.clone();
        let message_id = format!("msg_{}", Uuid::new_v4());
        let messages = Self::convert_messages_for_puter(request);
        let request_id = request_id.to_string();

        let client = self.client.clone();
        let cached_token = self.cached_token.clone();
        let username = self.username.clone();
        let password = self.password.clone();

        info!(
            "PUTER_STREAM: request_id={} model={} msgs={}",
            request_id,
            model_name,
            messages.len(),
        );

        async_stream::stream! {
            let mut sse = SSEBuilder::new(message_id, request_model, input_tokens);
            yield sse.message_start();

            // Get token (with retry on auth failure)
            let token = {
                let cached = cached_token.read().await;
                match &*cached {
                    Some(ct) if ct.obtained_at.elapsed().as_secs() < TOKEN_LIFETIME_SECS => {
                        ct.token.clone()
                    }
                    _ => {
                        drop(cached);
                        // Re-login
                        match do_login(&client, &username, &password).await {
                            Ok(new_token) => {
                                let mut cached = cached_token.write().await;
                                *cached = Some(CachedToken {
                                    token: new_token.clone(),
                                    obtained_at: Instant::now(),
                                });
                                new_token
                            }
                            Err(e) => {
                                error!("Puter auth failed: {}", e);
                                for event in sse.emit_error(&format!("Puter auth failed: {}", e)) {
                                    yield event;
                                }
                                yield sse.message_delta("end_turn", 0);
                                yield sse.message_stop();
                                return;
                            }
                        }
                    }
                }
            };

            // Attempt request, retry once on auth error
            let mut attempts = 0;
            let mut current_token = token;

            'auth_retry: loop {
                attempts += 1;
                if attempts > 2 {
                    for event in sse.emit_error("Puter: auth retry exhausted") {
                        yield event;
                    }
                    yield sse.message_delta("end_turn", 0);
                    yield sse.message_stop();
                    return;
                }

                let payload = PuterProvider::build_chat_payload(&current_token, &model_name, &messages, true);

                let resp = client
                    .post(CHAT_URL)
                    .header("Content-Type", "text/plain;actually=json")
                    .header("Origin", "http://127.0.0.1:8000")
                    .body(serde_json::to_string(&payload).unwrap())
                    .send()
                    .await;

                match resp {
                    Err(e) => {
                        error!("PUTER_STREAM_ERROR: request_id={} error={}", request_id, e);
                        for event in sse.emit_error(&format!("Puter connection error: {}", e)) {
                            yield event;
                        }
                        yield sse.message_delta("end_turn", 0);
                        yield sse.message_stop();
                        return;
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();

                        // Auth error → invalidate and retry
                        if status == 401 || status == 403 {
                            warn!("Puter auth error ({}), re-authenticating...", status);
                            match do_login(&client, &username, &password).await {
                                Ok(new_token) => {
                                    let mut cached = cached_token.write().await;
                                    *cached = Some(CachedToken {
                                        token: new_token.clone(),
                                        obtained_at: Instant::now(),
                                    });
                                    current_token = new_token;
                                    continue 'auth_retry;
                                }
                                Err(e) => {
                                    error!("Puter re-auth failed: {}", e);
                                    for event in sse.emit_error(&format!("Puter re-auth failed: {}", e)) {
                                        yield event;
                                    }
                                    yield sse.message_delta("end_turn", 0);
                                    yield sse.message_stop();
                                    return;
                                }
                            }
                        }

                        if status >= 400 {
                            let body_text = response.text().await.unwrap_or_default();
                            error!("Puter API error {}: {}", status, body_text);
                            for event in sse.emit_error(&format!("Puter error {}: {}", status, &body_text[..body_text.len().min(200)])) {
                                yield event;
                            }
                            yield sse.message_delta("end_turn", 0);
                            yield sse.message_stop();
                            return;
                        }

                        // ── Process streaming response ──
                        // Puter streams newline-delimited JSON objects:
                        // {"type": "reasoning", "reasoning": "..."} for thinking
                        // {"type": "text", "text": "..."} for text content
                        let mut think_parser = ThinkTagParser::new();
                        let mut byte_stream = response.bytes_stream();
                        let mut line_buffer = String::new();

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
                                let line = line_buffer[..newline_pos].trim().to_string();
                                line_buffer = line_buffer[newline_pos + 1..].to_string();

                                if line.is_empty() {
                                    continue;
                                }

                                let chunk: Value = match serde_json::from_str(&line) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        debug!("Puter: skipping unparseable chunk: {} ({})", &line[..line.len().min(100)], e);
                                        continue;
                                    }
                                };

                                let chunk_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");

                                match chunk_type {
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
                                    "text" => {
                                        if let Some(text) = chunk.get("text").and_then(|v| v.as_str())
                                            && !text.is_empty()
                                        {
                                            // Run through think tag parser for inline <think> tags
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
                                                        for event in sse.ensure_text_block() {
                                                            yield event;
                                                        }
                                                        yield sse.emit_text_delta(&c.content);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "error" => {
                                        let msg = chunk.get("message")
                                            .or_else(|| chunk.get("error"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Unknown Puter error");
                                        error!("Puter stream error: {}", msg);

                                        // Check if auth-related error
                                        let msg_lower = msg.to_lowercase();
                                        if msg_lower.contains("token") || msg_lower.contains("auth") || msg_lower.contains("login") {
                                            warn!("Puter token may be expired, invalidating...");
                                            let mut cached = cached_token.write().await;
                                            *cached = None;
                                        }

                                        for event in sse.emit_error(&format!("Puter: {}", msg)) {
                                            yield event;
                                        }
                                    }
                                    _ => {
                                        // Unknown type — check if it has content/text field as fallback
                                        if let Some(text) = chunk.get("text").and_then(|v| v.as_str())
                                            && !text.is_empty()
                                        {
                                            for event in sse.ensure_text_block() {
                                                yield event;
                                            }
                                            yield sse.emit_text_delta(text);
                                        }
                                    }
                                }
                            }
                        }

                        // Process remaining buffer
                        if !line_buffer.trim().is_empty()
                            && let Ok(chunk) = serde_json::from_str::<Value>(line_buffer.trim())
                        {
                            let chunk_type = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match chunk_type {
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
                                "text" => {
                                    if let Some(text) = chunk.get("text").and_then(|v| v.as_str())
                                        && !text.is_empty()
                                    {
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
                                                    for event in sse.ensure_text_block() {
                                                        yield event;
                                                    }
                                                    yield sse.emit_text_delta(&c.content);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
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
                                    for event in sse.ensure_text_block() {
                                        yield event;
                                    }
                                    yield sse.emit_text_delta(&remaining.content);
                                }
                            }
                        }

                        // Ensure at least one content block
                        if !sse.has_any_content() {
                            for event in sse.ensure_text_block() {
                                yield event;
                            }
                            yield sse.emit_text_delta(" ");
                        }

                        for event in sse.close_all_blocks() {
                            yield event;
                        }

                        let output_tokens = sse.estimate_output_tokens();
                        yield sse.message_delta("end_turn", output_tokens);
                        yield sse.message_stop();
                        return;
                    }
                }
            }
        }
    }

    pub async fn send_non_streaming(
        &self,
        request: &MessagesRequest,
        input_tokens: u32,
        request_id: &str,
    ) -> Result<Value, String> {
        let model_name = Settings::parse_model_name(
            request
                .resolved_provider_model
                .as_deref()
                .unwrap_or(&request.model),
        );

        let messages = Self::convert_messages_for_puter(request);

        info!(
            "PUTER_NON_STREAM: request_id={} model={}",
            request_id, model_name,
        );

        let token = self.ensure_token().await?;
        let payload = Self::build_chat_payload(&token, model_name, &messages, false);

        let response = self
            .client
            .post(CHAT_URL)
            .header("Content-Type", "text/plain;actually=json")
            .header("Origin", "http://127.0.0.1:8000")
            .body(serde_json::to_string(&payload).unwrap())
            .send()
            .await
            .map_err(|e| format!("Puter connection error: {}", e))?;

        let status = response.status().as_u16();

        // Retry on auth error
        if status == 401 || status == 403 {
            warn!("Puter auth error on non-streaming, re-authenticating...");
            self.invalidate_token().await;
            let new_token = self.ensure_token().await?;
            let payload = Self::build_chat_payload(&new_token, model_name, &messages, false);

            let response = self
                .client
                .post(CHAT_URL)
                .header("Content-Type", "text/plain;actually=json")
                .header("Origin", "http://127.0.0.1:8000")
                .body(serde_json::to_string(&payload).unwrap())
                .send()
                .await
                .map_err(|e| format!("Puter connection error: {}", e))?;

            let status = response.status().as_u16();
            if status >= 400 {
                let body_text = response.text().await.unwrap_or_default();
                return Err(format!(
                    "Puter error {}: {}",
                    status,
                    &body_text[..body_text.len().min(200)]
                ));
            }

            return self
                .parse_non_streaming_response(response, request, input_tokens)
                .await;
        }

        if status >= 400 {
            let body_text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Puter error {}: {}",
                status,
                &body_text[..body_text.len().min(200)]
            ));
        }

        self.parse_non_streaming_response(response, request, input_tokens)
            .await
    }

    async fn parse_non_streaming_response(
        &self,
        response: reqwest::Response,
        request: &MessagesRequest,
        input_tokens: u32,
    ) -> Result<Value, String> {
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("Puter response parse error: {}", e))?;

        let message_id = format!("msg_{}", Uuid::new_v4());
        let mut content_blocks: Vec<Value> = Vec::new();

        // Puter non-streaming response can be:
        // {"message": {"content": "...", "role": "assistant"}}
        // or direct {"text": "...", "reasoning": "..."}
        let text = body
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .or_else(|| body.get("text").and_then(|v| v.as_str()))
            .or_else(|| body.get("content").and_then(|v| v.as_str()));

        let reasoning = body.get("reasoning").and_then(|v| v.as_str()).or_else(|| {
            body.get("message")
                .and_then(|m| m.get("reasoning"))
                .and_then(|v| v.as_str())
        });

        if let Some(reasoning_text) = reasoning
            && !reasoning_text.is_empty()
        {
            content_blocks.push(json!({"type": "thinking", "thinking": reasoning_text}));
        }

        if let Some(text_content) = text
            && !text_content.is_empty()
        {
            // Parse out any inline <think> tags
            let mut think_parser = ThinkTagParser::new();
            let chunks = think_parser.feed(text_content);
            let mut text_parts = Vec::new();

            for c in chunks {
                match c.content_type {
                    ContentType::Thinking => {
                        content_blocks.push(json!({"type": "thinking", "thinking": c.content}));
                    }
                    ContentType::Text => {
                        text_parts.push(c.content);
                    }
                }
            }

            if let Some(remaining) = think_parser.flush() {
                match remaining.content_type {
                    ContentType::Thinking => {
                        content_blocks
                            .push(json!({"type": "thinking", "thinking": remaining.content}));
                    }
                    ContentType::Text => {
                        text_parts.push(remaining.content);
                    }
                }
            }

            let combined_text = text_parts.join("");
            if !combined_text.is_empty() {
                content_blocks.push(json!({"type": "text", "text": combined_text}));
            }
        }

        if content_blocks.is_empty() {
            content_blocks.push(json!({"type": "text", "text": " "}));
        }

        let output_tokens = text.map(|t| (t.len() as u32 / 4).max(1)).unwrap_or(1);

        Ok(json!({
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "model": request.model,
            "content": content_blocks,
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }))
    }
}

/// Standalone login function with retry, usable from async_stream context.
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
