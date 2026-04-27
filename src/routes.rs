use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use uuid::Uuid;

use crate::auth::validate_api_key;
use crate::config::Settings;
use crate::converter::count_request_tokens;
use crate::models::anthropic::{MessagesRequest, TokenCountRequest};
use crate::optimization::try_optimizations;
use crate::providers::OpenAICompatProvider;

pub struct AppState {
    pub settings: Settings,
    pub provider: OpenAICompatProvider,
}

#[allow(clippy::result_large_err)]
fn check_auth(headers: &HeaderMap, settings: &Settings) -> Result<(), Response> {
    validate_api_key(headers, &settings.anthropic_auth_token).map_err(|msg| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "type": "error",
                "error": {"type": "authentication_error", "message": msg}
            })),
        )
            .into_response()
    })
}

pub async fn create_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Json(mut request): axum::extract::Json<MessagesRequest>,
) -> Response {
    tracing::trace!(
        "Incoming JSON payload: {}",
        serde_json::to_string(&request).unwrap_or_default()
    );

    if let Err(r) = check_auth(&headers, &state.settings) {
        return r;
    }

    if request.messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "type": "error",
                "error": {"type": "invalid_request_error", "message": "messages cannot be empty"}
            })),
        )
            .into_response();
    }

    request.original_model = Some(request.model.clone());
    let resolved = state.settings.resolve_model(&request.model);
    request.resolved_provider_model = Some(resolved.clone());
    request.model = Settings::parse_model_name(&resolved).to_string();

    if let Some(optimized) = try_optimizations(&request, &state.settings) {
        return Json(optimized).into_response();
    }

    let request_id = format!("req_{}", &Uuid::new_v4().to_string()[..12]);
    let input_tokens = count_request_tokens(&request);

    // Non-streaming path (fallback — only when client explicitly sets stream: false)
    if request.stream == Some(false) {
        let result = state
            .provider
            .send_non_streaming(&request, input_tokens, &request_id)
            .await;
        return match result {
            Ok(response_json) => Json(response_json).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "type": "error",
                    "error": {"type": "api_error", "message": e}
                })),
            )
                .into_response(),
        };
    }

    // Default: SSE streaming path (existing behavior, unchanged)
    let stream = state
        .provider
        .stream_response(&request, input_tokens, &request_id);

    let body_stream = tokio_stream::StreamExt::map(stream, Ok::<_, std::convert::Infallible>);

    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream))
        .unwrap()
}

pub async fn count_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TokenCountRequest>,
) -> Response {
    tracing::trace!(
        "Incoming count_tokens JSON payload: {}",
        serde_json::to_string(&request).unwrap_or_default()
    );

    if let Err(r) = check_auth(&headers, &state.settings) {
        return r;
    }

    let messages_request = MessagesRequest {
        model: request.model,
        max_tokens: None,
        messages: request.messages,
        system: request.system,
        stop_sequences: None,
        stream: None,
        temperature: None,
        top_p: None,
        top_k: None,
        metadata: None,
        tools: request.tools,
        tool_choice: None,
        thinking: None,
        extra_body: None,
        original_model: None,
        resolved_provider_model: None,
    };

    let tokens = count_request_tokens(&messages_request);
    Json(json!({"input_tokens": tokens})).into_response()
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let provider_type = Settings::parse_provider_type(&state.settings.model);
    Json(json!({
        "status": "healthy",
        "model": state.settings.model,
        "provider": provider_type,
        "version": env!("CARGO_PKG_VERSION"),
        "features": {
            "ip_rotation": state.settings.enable_ip_rotation,
            "tool_retry": state.settings.enable_tool_retry,
        }
    }))
}

pub async fn root(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&headers, &state.settings) {
        return r;
    }

    let provider_type = Settings::parse_provider_type(&state.settings.model);
    Json(json!({
        "status": "ok",
        "provider": provider_type,
        "model": state.settings.model,
    }))
    .into_response()
}
