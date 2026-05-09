mod auth;
mod config;
mod config_loader;
mod config_watcher;
mod converter;
mod heuristic_tool_parser;
mod ip_rotator;
mod key_pool;
mod model_router;
mod models;
mod optimization;
mod panel;
mod panel_config;
mod providers;
mod rate_limiter;
mod routes;
mod rtk;
mod sse;
mod think_parser;
mod tool_intent_detector;

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::routing::{get, post};
use tokio::sync::broadcast;
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Settings;
use key_pool::KeyPoolManager;
use model_router::ModelRouter;
use panel_config::PanelConfig;
use providers::OpenAICompatProvider;
use routes::AppState;

const BANNER: &str = "\n\
\x1b[36m  ██████╗██████╗ ██╗   ██╗███████╗████████╗██████╗ ██╗  ██╗██╗   ██╗\n\
 ██╔════╝██╔══██╗██║   ██║██╔════╝╚══██╔══╝██╔══██╗╚██╗██╔╝╚██╗ ██╔╝\n\
 ██║     ██████╔╝██║   ██║███████╗   ██║   ██║  ██║ ╚███╔╝  ╚████╔╝ \n\
 ██║     ██╔══██╗██║   ██║╚════██║   ██║   ██║  ██║ ██╔██╗   ╚██╔╝  \n\
 ╚██████╗██║  ██║╚██████╔╝███████║   ██║   ██████╔╝██╔╝ ██╗   ██║   \n\
  ╚═════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝   ╚═════╝ ╚═╝  ╚═╝   ╚═╝   \n\
\x1b[0m            \x1b[1;33mIntelligent Proxy Router for Claude Code\x1b[0m 🚀\n";

const SETUP_BANNER: &str = "\n\
\x1b[1;33m  ⚡ CRUSTOXY — FIRST TIME SETUP\x1b[0m\n\
  ────────────────────────────────\n\
  No configuration found.\n\
  \n\
  Open the dashboard to configure:\n\
  → \x1b[4;36mhttp://{}:{}/ui/\x1b[0m\n\
  \n\
  Set up your models and API keys,\n\
  then click \x1b[1mSAVE & APPLY\x1b[0m to begin.\n";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    println!("{}", BANNER);

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // ── Load configuration ───────────────────────────────────────────────
    let config_path = config_loader::config_path();
    let load_result = config_loader::load_or_create(&config_path);
    let is_configured = load_result.is_configured();
    let panel_config = load_result.into_config();

    // Build runtime settings from panel config
    let settings = Settings::from_panel_config(&panel_config);

    if !is_configured {
        println!(
            "{}",
            SETUP_BANNER.replacen("{}", &settings.host, 1).replacen(
                "{}",
                &settings.port.to_string(),
                1
            )
        );
    }

    // ── Initialize engines ───────────────────────────────────────────────
    let active_profile = panel_config.active_profile().clone();
    let key_pool_manager = Arc::new(KeyPoolManager::from_config(&active_profile));
    let model_router = Arc::new(ModelRouter::from_config(&active_profile));

    // Spawn key pool recovery task
    let recovery_interval = active_profile.routing.health_recovery_interval;
    key_pool_manager.spawn_recovery_task(recovery_interval);

    // ── Initialize providers ─────────────────────────────────────────────
    let provider = OpenAICompatProvider::new(&settings);

    let puter_provider = if let Some(ref creds) = settings.puter_api_key {
        info!("Puter provider detected, initializing...");
        match providers::PuterProvider::new(creds, &settings).await {
            Ok(pp) => {
                info!("Puter provider ready");
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
        info!("Kimi OAuth provider detected, initializing...");
        match providers::kimi_oauth::bootstrap_if_enabled(&settings).await {
            Ok(Some(p)) => {
                info!("Kimi OAuth provider ready");
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
        info!("Cloudflare provider detected, initializing...");
        Some(Arc::new(providers::CloudflareProvider::new(&settings)))
    } else {
        None
    };

    // ── Build shared state ───────────────────────────────────────────────
    let state = Arc::new(AppState {
        settings: ArcSwap::from_pointee(settings.clone()),
        panel_config: ArcSwap::from_pointee(panel_config),
        key_pool_manager: key_pool_manager.clone(),
        model_router: model_router.clone(),
        provider,
        puter_provider,
        kimi_oauth_provider,
        cloudflare_provider,
    });

    // ── Config watcher for hot-reload ────────────────────────────────────
    let (config_tx, mut config_rx) = broadcast::channel::<PanelConfig>(4);
    config_watcher::spawn_config_watcher(config_path, config_tx);

    // Spawn reload handler
    let reload_state = state.clone();
    tokio::spawn(async move {
        loop {
            match config_rx.recv().await {
                Ok(new_config) => {
                    info!("Hot-reload: applying new configuration...");
                    let new_settings = Settings::from_panel_config(&new_config);
                    let active = new_config.active_profile().clone();

                    reload_state.settings.store(Arc::new(new_settings));
                    reload_state.panel_config.store(Arc::new(new_config));
                    reload_state.key_pool_manager.reload(&active).await;
                    reload_state.model_router.reload(&active).await;

                    // Sync WARP state
                    ip_rotator::sync_warp_state(active.features.enable_ip_rotation).await;

                    info!("Hot-reload: configuration applied successfully");
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Config reload lagged by {} events", n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Config watcher channel closed");
                    break;
                }
            }
        }
    });

    // ── Build router ─────────────────────────────────────────────────────
    let app = Router::new()
        .route("/v1/messages", post(routes::create_message))
        .route("/v1/messages/count_tokens", post(routes::count_tokens))
        .route("/health", get(routes::health))
        .route("/", get(routes::root))
        // Crustoxy-Panel routes
        .nest("/ui", panel::ui_routes())
        .nest("/api", panel::api_routes())
        .with_state(state);

    // ── Start server ─────────────────────────────────────────────────────
    let addr = format!("{}:{}", settings.host, settings.port);
    info!("Crustoxy starting on {}", addr);
    info!("Dashboard: http://{}:{}/ui", settings.host, settings.port);
    info!("Default model: {}", settings.model);
    info!(
        "IP rotation: {}",
        if settings.enable_ip_rotation {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!("Config: {}", config_loader::config_path().display());

    // Initial WARP sync
    ip_rotator::sync_warp_state(settings.enable_ip_rotation).await;

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Received shutdown signal");
        })
        .await
        .unwrap();
}
