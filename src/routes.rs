use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{debug, trace};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::auth::validate_api_key;
use crate::config::Settings;
use crate::converter::count_request_tokens;
use crate::key_pool::KeyPoolManager;
use crate::model_router::ModelRouter;
use crate::models::anthropic::{
    ContentBlock, MessageContent, MessagesRequest, SystemPrompt, TokenCountRequest,
    extract_text_from_system,
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
    pub provider: RwLock<Arc<OpenAICompatProvider>>,
    pub puter_provider: RwLock<Option<Arc<PuterProvider>>>,
    pub kimi_oauth_provider: RwLock<Option<Arc<crate::providers::kimi_oauth::KimiOauthProvider>>>,
    pub cloudflare_provider: RwLock<Option<Arc<crate::providers::CloudflareProvider>>>,
}

pub struct ProviderBundle {
    pub provider: Arc<OpenAICompatProvider>,
    pub puter_provider: Option<Arc<PuterProvider>>,
    pub kimi_oauth_provider: Option<Arc<crate::providers::kimi_oauth::KimiOauthProvider>>,
    pub cloudflare_provider: Option<Arc<crate::providers::CloudflareProvider>>,
}

pub async fn build_provider_bundle(settings: &Settings) -> ProviderBundle {
    let provider = Arc::new(OpenAICompatProvider::new(settings));

    let puter_provider = if let Some(ref creds) = settings.puter_api_key {
        tracing::info!("Puter provider detected, initializing...");
        match crate::providers::PuterProvider::new(creds, settings).await {
            Ok(pp) => {
                tracing::info!("Puter provider ready");
                Some(Arc::new(pp))
            }
            Err(e) => {
                tracing::error!("Failed to initialize Puter provider: {}", e);
                None
            }
        }
    } else {
        None
    };

    let kimi_oauth_provider = if settings.kimi_oauth_enable {
        tracing::info!("Kimi OAuth provider detected, initializing...");
        match crate::providers::kimi_oauth::bootstrap_if_enabled(settings).await {
            Ok(Some(p)) => {
                tracing::info!("Kimi OAuth provider ready");
                Some(p)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!("Failed to initialize Kimi OAuth: {}", e);
                None
            }
        }
    } else {
        None
    };

    let cloudflare_provider = if settings.cloudflare_api_key.is_some() {
        tracing::info!("Cloudflare provider detected, initializing...");
        Some(Arc::new(crate::providers::CloudflareProvider::new(
            settings,
        )))
    } else {
        None
    };

    ProviderBundle {
        provider,
        puter_provider,
        kimi_oauth_provider,
        cloudflare_provider,
    }
}

impl AppState {
    pub async fn rebuild_providers(&self, settings: &Settings) {
        let bundle = build_provider_bundle(settings).await;
        *self.provider.write().await = bundle.provider;
        *self.puter_provider.write().await = bundle.puter_provider;
        *self.kimi_oauth_provider.write().await = bundle.kimi_oauth_provider;
        *self.cloudflare_provider.write().await = bundle.cloudflare_provider;
        tracing::info!("Provider runtime reloaded");
    }
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
        "Incoming request: model={} messages={} tools={}",
        request.model,
        request.messages.len(),
        request.tools.as_ref().map_or(0, Vec::len)
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

    if request_has_images(&request) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "image content blocks are not supported by this OpenAI-compatible proxy path yet"
                }
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
                trace!(
                    "System prompt transformed: original_chars={} transformed_chars={}",
                    orig_len, new_len
                );
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
        let puter_provider = state.puter_provider.read().await.clone();
        if let Some(ref pp) = puter_provider {
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
                        "message": "Puter provider not enabled. Add a Puter key in the panel config."
                    }
                })),
            )
                .into_response();
        }
    } else if provider_type == "kimi_oauth" {
        let kimi_oauth_provider = state.kimi_oauth_provider.read().await.clone();
        if let Some(ref kimi_provider) = kimi_oauth_provider {
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
                        "message": "Kimi OAuth provider not enabled. Add a Kimi OAuth key in the panel config."
                    }
                })),
            )
                .into_response();
        }
    } else if provider_type == "cloudflare" {
        let cloudflare_provider = state.cloudflare_provider.read().await.clone();
        if let Some(ref cloudflare_provider) = cloudflare_provider {
            if request.stream == Some(false) {
                let result = cloudflare_provider
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

            let stream = cloudflare_provider.stream_response(
                &request,
                input_tokens,
                &request_id,
                state.key_pool_manager.clone(),
                state.model_router.clone(),
            );
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
                        "message": "Cloudflare provider not enabled. Add a Cloudflare account_id:api_token key in the panel config."
                    }
                })),
            )
                .into_response();
        }
    }

    // Non-streaming path (fallback — only when client explicitly sets stream: false)
    if request.stream == Some(false) {
        let provider = state.provider.read().await.clone();
        let result = provider
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
    let provider = state.provider.read().await.clone();
    let stream = provider.stream_response(
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

    let puter_status = if state.puter_provider.read().await.is_some() {
        "enabled"
    } else {
        "disabled"
    };

    let kimi_status = if state.kimi_oauth_provider.read().await.is_some() {
        "enabled"
    } else {
        "disabled"
    };

    let cloudflare_status = if state.cloudflare_provider.read().await.is_some() {
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

fn request_has_images(request: &MessagesRequest) -> bool {
    request.messages.iter().any(|msg| match &msg.content {
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. })),
        MessageContent::Text(_) => false,
    })
}
