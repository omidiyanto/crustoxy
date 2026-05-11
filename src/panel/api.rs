//! REST API handlers for Crustoxy-Panel configuration management.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use crate::auth::constant_time_eq;
use crate::config::{PROVIDERS, Settings};
use crate::panel_config::{PanelConfig, ProfileConfig};
use crate::routes::AppState;

/// Check panel authentication.
/// If `ANTHROPIC_AUTH_TOKEN` is set, require it. Otherwise, open access.
#[allow(clippy::result_large_err)]
fn check_panel_auth(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    let settings = state.settings.load();
    let token = match &settings.anthropic_auth_token {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(()), // No auth required
    };

    // Check Authorization header, cookie, or query parameter
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
        .or_else(|| headers.get("x-panel-token").and_then(|v| v.to_str().ok()));

    match provided {
        Some(t) if constant_time_eq(t.as_bytes(), token.as_bytes()) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Authentication required"})),
        )
            .into_response()),
    }
}

/// POST /api/auth — Authenticate with token.
#[derive(Deserialize)]
pub struct AuthRequest {
    token: String,
}

pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthRequest>,
) -> Response {
    let settings = state.settings.load();
    let expected = settings.anthropic_auth_token.as_deref().unwrap_or("");

    if expected.is_empty() || constant_time_eq(body.token.as_bytes(), expected.as_bytes()) {
        Json(json!({"authenticated": true})).into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid token"})),
        )
            .into_response()
    }
}

/// GET /api/config — Get current configuration.
pub async fn get_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }
    let config = state.panel_config.load();
    Json(json!(masked_config(&config))).into_response()
}

/// PUT /api/config — Update entire configuration.
pub async fn update_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(mut new_config): Json<PanelConfig>,
) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let current = state.panel_config.load();
    preserve_masked_provider_keys(&mut new_config, &current);
    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid config: {}", e)})),
        )
            .into_response();
    }
    if let Err(e) = validate_runtime_settings(&new_config) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid runtime config: {}", e)})),
        )
            .into_response();
    }

    // Save to disk
    let config_path = crate::config_loader::config_path();
    if let Err(e) = new_config.save(&config_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save config: {}", e)})),
        )
            .into_response();
    }

    // Apply immediately (hot-reload)
    if let Err(e) = apply_config(&state, new_config).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Failed to apply config: {}", e)})),
        )
            .into_response();
    }

    info!("Config updated via panel API");
    Json(json!({"status": "applied"})).into_response()
}

/// GET /api/profiles — List all profiles.
pub async fn list_profiles(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }
    let config = state.panel_config.load();
    let profiles: Vec<serde_json::Value> = config
        .profiles
        .iter()
        .map(|(key, p)| {
            json!({
                "key": key,
                "name": p.name,
                "active": key == &config.general.active_profile,
            })
        })
        .collect();
    Json(json!({"profiles": profiles})).into_response()
}

/// POST /api/profiles — Create a new profile.
#[derive(Deserialize)]
pub struct CreateProfileRequest {
    key: String,
    name: String,
}

pub async fn create_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateProfileRequest>,
) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let mut config = (**state.panel_config.load()).clone();

    if config.profiles.contains_key(&body.key) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Profile already exists"})),
        )
            .into_response();
    }

    let profile = ProfileConfig {
        name: body.name,
        ..Default::default()
    };
    config.profiles.insert(body.key.clone(), profile);

    save_and_apply(&state, config).await
}

/// PUT /api/profiles/{name} — Update a specific profile.
pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(mut profile): Json<ProfileConfig>,
) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let mut config = (**state.panel_config.load()).clone();

    if !config.profiles.contains_key(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Profile not found"})),
        )
            .into_response();
    }

    if let Some(current_profile) = config.profiles.get(&name) {
        preserve_masked_keys_for_profile(&mut profile, current_profile);
    }

    config.profiles.insert(name, profile);
    save_and_apply(&state, config).await
}

/// DELETE /api/profiles/{name} — Delete a profile.
pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let mut config = (**state.panel_config.load()).clone();

    if config.profiles.len() <= 1 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Cannot delete the last profile"})),
        )
            .into_response();
    }

    if !config.profiles.contains_key(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Profile not found"})),
        )
            .into_response();
    }

    config.profiles.remove(&name);

    // If deleted profile was active, switch to first available
    if config.general.active_profile == name {
        config.general.active_profile = config.profiles.keys().next().cloned().unwrap_or_default();
    }

    save_and_apply(&state, config).await
}

/// POST /api/profiles/{name}/activate — Switch active profile.
pub async fn activate_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let mut config = (**state.panel_config.load()).clone();

    if !config.profiles.contains_key(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Profile not found"})),
        )
            .into_response();
    }

    config.general.active_profile = name.clone();
    info!("Switching active profile to: {}", name);
    save_and_apply(&state, config).await
}

/// GET /api/status — System status including key pool and model router health.
pub async fn get_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let settings = state.settings.load();
    let config = state.panel_config.load();

    let key_status = state.key_pool_manager.status().await;
    let model_status = state.model_router.status().await;

    Json(json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "active_profile": config.general.active_profile,
        "default_model": settings.model,
        "features": {
            "ip_rotation": settings.enable_ip_rotation,
            "tool_retry": settings.enable_tool_retry,
            "rtk": settings.enable_rtk,
        },
        "key_pools": key_status,
        "model_router": model_status,
    }))
    .into_response()
}

/// POST /api/restart — Trigger a configuration reload.
pub async fn trigger_restart(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let config_path = crate::config_loader::config_path();
    match PanelConfig::load(&config_path) {
        Ok(config) => {
            if let Err(e) = apply_config(&state, config).await {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("Failed to apply config: {}", e)})),
                )
                    .into_response();
            }
            info!("Configuration reloaded via panel restart");
            Json(json!({"status": "restarted"})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to reload config: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /api/providers — List all known providers with default base URLs.
pub async fn list_providers(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_panel_auth(&headers, &state) {
        return r;
    }

    let providers: Vec<serde_json::Value> = PROVIDERS
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "default_base_url": p.default_base_url,
            })
        })
        .collect();

    Json(json!({"providers": providers})).into_response()
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Save config to disk and apply it immediately.
async fn save_and_apply(state: &AppState, config: PanelConfig) -> Response {
    if let Err(e) = config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid config: {}", e)})),
        )
            .into_response();
    }
    if let Err(e) = validate_runtime_settings(&config) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid runtime config: {}", e)})),
        )
            .into_response();
    }

    let config_path = crate::config_loader::config_path();
    if let Err(e) = config.save(&config_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {}", e)})),
        )
            .into_response();
    }
    if let Err(e) = apply_config(state, config).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Failed to apply config: {}", e)})),
        )
            .into_response();
    }
    Json(json!({"status": "applied"})).into_response()
}

/// Apply a new config: update settings, key pools, and model router.
async fn apply_config(state: &AppState, config: PanelConfig) -> Result<(), String> {
    let new_settings = Settings::from_panel_config(&config);
    new_settings.validate_runtime_security()?;
    let active = config.active_profile().clone();

    state.settings.store(Arc::new(new_settings.clone()));
    state.panel_config.store(Arc::new(config));
    state.key_pool_manager.reload(&active).await;
    state.model_router.reload(&active).await;
    state.rebuild_providers(&new_settings).await;

    info!("Configuration applied successfully");
    Ok(())
}

fn validate_runtime_settings(config: &PanelConfig) -> Result<(), String> {
    Settings::from_panel_config(config).validate_runtime_security()
}

fn masked_config(config: &PanelConfig) -> PanelConfig {
    let mut masked = config.clone();
    for profile in masked.profiles.values_mut() {
        for value in profile.provider_keys.values_mut() {
            *value = mask_key_list(value);
        }
    }
    masked
}

fn mask_key_list(raw: &str) -> String {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::key_pool::mask_key)
        .collect::<Vec<_>>()
        .join(" ; ")
}

fn preserve_masked_provider_keys(new_config: &mut PanelConfig, current: &PanelConfig) {
    for (profile_key, new_profile) in &mut new_config.profiles {
        if let Some(current_profile) = current.profiles.get(profile_key) {
            preserve_masked_keys_for_profile(new_profile, current_profile);
        }
    }
}

fn preserve_masked_keys_for_profile(
    new_profile: &mut ProfileConfig,
    current_profile: &ProfileConfig,
) {
    for (provider, new_raw) in &mut new_profile.provider_keys {
        let Some(current_raw) = current_profile.provider_keys.get(provider) else {
            continue;
        };
        *new_raw = merge_masked_key_list(new_raw, current_raw);
    }
}

fn merge_masked_key_list(new_raw: &str, current_raw: &str) -> String {
    let new_parts: Vec<String> = new_raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    let current_parts: Vec<String> = current_raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    if new_parts.is_empty() {
        return String::new();
    }

    new_parts
        .iter()
        .enumerate()
        .map(|(idx, candidate)| {
            current_parts
                .get(idx)
                .filter(|old| candidate == &crate::key_pool::mask_key(old))
                .cloned()
                .unwrap_or_else(|| candidate.clone())
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}
