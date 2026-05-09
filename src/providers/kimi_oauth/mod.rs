pub mod auth;
pub mod provider;
pub mod translate;

use crate::config::Settings;
use auth::{AuthManager, run_device_login};
pub use provider::KimiOauthProvider;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub async fn bootstrap_if_enabled(
    settings: &Settings,
) -> Result<Option<Arc<KimiOauthProvider>>, String> {
    if !settings.kimi_oauth_enable {
        return Ok(None);
    }
    if !any_model_uses_kimi_oauth(settings) {
        info!("kimi_oauth: enabled but no MODEL slot routes to it; skipping auth");
        return Ok(None);
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .default_headers(auth::build_common_headers())
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let auth_manager = Arc::new(AuthManager::new(client.clone()));

    // Eager auth check
    match auth_manager.try_load_existing().await {
        Ok(Some(_)) => info!("kimi_oauth: existing auth loaded"),
        _ => {
            info!("kimi_oauth: no valid auth found, starting device login");
            let tokens = run_device_login(&client).await?;
            auth_manager.persist_initial(tokens).await?;
            info!("kimi_oauth: device login complete, token saved");
        }
    }

    Ok(Some(Arc::new(KimiOauthProvider::new(
        settings.clone(),
        auth_manager,
    ))))
}

fn any_model_uses_kimi_oauth(settings: &Settings) -> bool {
    if settings.model.starts_with("kimi_oauth/") {
        return true;
    }
    if let Some(ref m) = settings.model_opus
        && m.starts_with("kimi_oauth/")
    {
        return true;
    }
    if let Some(ref m) = settings.model_sonnet
        && m.starts_with("kimi_oauth/")
    {
        return true;
    }
    if let Some(ref m) = settings.model_haiku
        && m.starts_with("kimi_oauth/")
    {
        return true;
    }
    false
}
