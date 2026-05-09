use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{debug, trace};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use uuid::Uuid;

use crate::auth::validate_api_key;
use crate::config::Settings;
use crate::converter::count_request_tokens;
use crate::key_pool::KeyPoolManager;
use crate::model_router::ModelRouter;
use crate::models::anthropic::{
    MessagesRequest, SystemPrompt, TokenCountRequest, extract_text_from_system,
};
use crate::optimization::try_optimizations;
use crate::panel_config::PanelConfig;
use crate::providers::OpenAICompatProvider;
use crate::providers::PuterProvider;
use crate::rtk;

/// Shared application state with hot-reloadable configuration.
pub struct AppState {
    pub settings: ArcSwap<Settings>,
    pub panel_config: ArcSwap<PanelConfig>,
    pub key_pool_manager: Arc<KeyPoolManager>,
    pub model_router: Arc<ModelRouter>,
    pub provider: OpenAICompatProvider,
    pub puter_provider: Option<Arc<PuterProvider>>,
    pub kimi_oauth_provider: Option<Arc<crate::providers::kimi_oauth::KimiOauthProvider>>,
    pub cloudflare_provider: Option<Arc<crate::providers::CloudflareProvider>>,
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
    trace!(
        "Incoming request payload: {}",
        serde_json::to_string(&request).unwrap_or_default()
    );
    let settings = state.settings.load();
    if let Err(r) = check_auth(&headers, &settings) {
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

    // Resolve model via ModelRouter (multi-model routing)
    request.original_model = Some(request.model.clone());
    let resolved = match state.model_router.resolve(&request.model).await {
        Some(r) => r.endpoint.full_spec.clone(),
        None => settings.resolve_model(&request.model),
    };
    request.resolved_provider_model = Some(resolved.clone());
    request.model = Settings::parse_model_name(&resolved).to_string();

    // Apply system prompt transformations (RTK compaction / override)
    if settings.override_system_prompt.is_some() || settings.enable_rtk {
        let sys_text = extract_text_from_system(&request.system);
        if !sys_text.is_empty() {
            let transformed = rtk::apply_system_prompt_transform(
                &sys_text,
                &settings.override_system_prompt,
                settings.enable_rtk,
            );

            let orig_len = sys_text.len();
            let new_len = transformed.len();
            if orig_len != new_len {
                debug!(
                    "RTK applied: system prompt compacted from {} to {} chars",
                    orig_len, new_len
                );
                trace!("Original system prompt:\n{}", sys_text);
                trace!("Transformed system prompt:\n{}", transformed);
            }

            request.system = Some(SystemPrompt::Text(transformed));
        }
    }

    if let Some(optimized) = try_optimizations(&request, &settings) {
        return Json(optimized).into_response();
    }

    let request_id = format!("req_{}", &Uuid::new_v4().to_string()[..12]);
    let input_tokens = count_request_tokens(&request);

    // Check if this request should go to a special provider
    let provider_type = request
        .resolved_provider_model
        .as_deref()
        .map(Settings::parse_provider_type)
        .unwrap_or("");

    if provider_type == "puter" {
        if let Some(ref pp) = state.puter_provider {
            if request.stream == Some(false) {
                let result = pp
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

            let stream = pp.stream_response(&request, input_tokens, &request_id);
            let body_stream =
                tokio_stream::StreamExt::map(stream, Ok::<_, std::convert::Infallible>);

            return Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .header("X-Accel-Buffering", "no")
                .body(Body::from_stream(body_stream))
                .unwrap();
        } else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": "Puter provider not enabled. Set PUTER_API_KEY to enable."
                    }
                })),
            )
                .into_response();
        }
    } else if provider_type == "kimi_oauth" {
        if let Some(ref kimi_provider) = state.kimi_oauth_provider {
            if request.stream == Some(false) {
                let result = kimi_provider
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

            let stream = kimi_provider.stream_response(&request, input_tokens, &request_id);
            let body_stream =
                tokio_stream::StreamExt::map(stream, Ok::<_, std::convert::Infallible>);

            return Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .header("X-Accel-Buffering", "no")
                .body(Body::from_stream(body_stream))
                .unwrap();
        } else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": "Kimi OAuth provider not enabled. Set KIMI_OAUTH_ENABLE=true."
                    }
                })),
            )
                .into_response();
        }
    } else if provider_type == "cloudflare" {
        if let Some(ref cloudflare_provider) = state.cloudflare_provider {
            if request.stream == Some(false) {
                let result = cloudflare_provider
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

            let stream = cloudflare_provider.stream_response(&request, input_tokens, &request_id);
            let body_stream =
                tokio_stream::StreamExt::map(stream, Ok::<_, std::convert::Infallible>);

            return Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .header("X-Accel-Buffering", "no")
                .body(Body::from_stream(body_stream))
                .unwrap();
        } else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": "Cloudflare provider not enabled. Set CLOUDFLARE_API_KEY to enable."
                    }
                })),
            )
                .into_response();
        }
    }

    // Non-streaming path (fallback — only when client explicitly sets stream: false)
    if request.stream == Some(false) {
        let result = state
            .provider
            .send_non_streaming(
                &request,
                input_tokens,
                &request_id,
                state.key_pool_manager.clone(),
                state.model_router.clone(),
            )
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

    // Default: SSE streaming path
    let stream = state.provider.stream_response(
        &request,
        input_tokens,
        &request_id,
        state.key_pool_manager.clone(),
        state.model_router.clone(),
    );

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
    let settings = state.settings.load();
    if let Err(r) = check_auth(&headers, &settings) {
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
        output_config: None,
        extra_body: None,
        original_model: None,
        resolved_provider_model: None,
    };

    let tokens = count_request_tokens(&messages_request);
    Json(json!({"input_tokens": tokens})).into_response()
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = state.settings.load();
    let config = state.panel_config.load();
    let provider_type = Settings::parse_provider_type(&settings.model);

    let puter_status = if state.puter_provider.is_some() {
        "enabled"
    } else {
        "disabled"
    };

    let kimi_status = if state.kimi_oauth_provider.is_some() {
        "enabled"
    } else {
        "disabled"
    };

    let cloudflare_status = if state.cloudflare_provider.is_some() {
        "enabled"
    } else {
        "disabled"
    };

    Json(json!({
        "status": "healthy",
        "model": settings.model,
        "provider": provider_type,
        "version": env!("CARGO_PKG_VERSION"),
        "active_profile": config.general.active_profile,
        "features": {
            "ip_rotation": settings.enable_ip_rotation,
            "tool_retry": settings.enable_tool_retry,
            "rtk": settings.enable_rtk,
            "puter": puter_status,
            "kimi_oauth": kimi_status,
            "cloudflare": cloudflare_status,
        }
    }))
}

pub async fn root(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let settings = state.settings.load();
    if let Err(r) = check_auth(&headers, &settings) {
        return r;
    }

    let provider_type = Settings::parse_provider_type(&settings.model);
    Json(json!({
        "status": "ok",
        "provider": provider_type,
        "model": settings.model,
    }))
    .into_response()
}
