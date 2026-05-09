//! Crustoxy-Panel — embedded web dashboard served at `/ui/*`.
//!
//! Provides REST API endpoints at `/api/*` for configuration management
//! and serves static HTML/CSS/JS assets embedded in the binary.

pub mod api;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::Embed;
use std::sync::Arc;

use crate::routes::AppState;

#[derive(Embed)]
#[folder = "src/panel/assets/"]
struct PanelAssets;

/// Build the UI routes for serving embedded static assets.
pub fn ui_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(serve_index))
        .route("/{*path}", get(serve_asset))
}

/// Build the API routes for config management.
pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth", axum::routing::post(api::authenticate))
        .route("/config", get(api::get_config).put(api::update_config))
        .route(
            "/profiles",
            get(api::list_profiles).post(api::create_profile),
        )
        .route(
            "/profiles/{name}",
            axum::routing::put(api::update_profile).delete(api::delete_profile),
        )
        .route(
            "/profiles/{name}/activate",
            axum::routing::post(api::activate_profile),
        )
        .route("/status", get(api::get_status))
        .route("/restart", axum::routing::post(api::trigger_restart))
        .route("/providers", get(api::list_providers))
}

/// Serve the main index.html page.
async fn serve_index(State(state): State<Arc<AppState>>) -> Response {
    // Check auth for panel access
    if let Some(ref token) = state.settings.load().anthropic_auth_token
        && !token.is_empty()
    {
        // Auth is required — serve index.html (client-side will handle auth flow)
    }

    match PanelAssets::get("index.html") {
        Some(file) => Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(file.data.to_vec()))
            .unwrap(),
        None => (StatusCode::NOT_FOUND, "Panel assets not found").into_response(),
    }
}

/// Serve a static asset file (CSS, JS, SVG, etc.).
async fn serve_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    match PanelAssets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "public, max-age=3600")
                .body(Body::from(file.data.to_vec()))
                .unwrap()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}
