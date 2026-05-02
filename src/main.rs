mod auth;
mod config;
mod converter;
mod heuristic_tool_parser;
mod ip_rotator;
mod models;
mod optimization;
mod providers;
mod rate_limiter;
mod routes;
mod rtk;
mod sse;
mod think_parser;
mod tool_intent_detector;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Settings;
use providers::OpenAICompatProvider;
use routes::AppState;

const BANNER: &str = "\n\
\x1b[36m  ██████╗██████╗ ██╗   ██╗███████╗████████╗██████╗ ██╗  ██╗██╗   ██╗\n\
 ██╔════╝██╔══██╗██║   ██║██╔════╝╚══██╔══╝██╔══██╗╚██╗██╔╝╚██╗ ██╔╝\n\
 ██║     ██████╔╝██║   ██║███████╗   ██║   ██║  ██║ ╚███╔╝  ╚████╔╝ \n\
 ██║     ██╔══██╗██║   ██║╚════██║   ██║   ██║  ██║ ██╔██╗   ╚██╔╝  \n\
 ╚██████╗██║  ██║╚██████╔╝███████║   ██║   ██████╔╝██╔╝ ██╗   ██║   \n\
  ╚═════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝   ╚═════╝ ╚═╝  ╚═╝   ╚═╝   \n\
\x1b[0m            \x1b[1;33mProxy Router for Your Claude Code\x1b[0m 🚀\n";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    println!("{}", BANNER);

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env();
    let provider = OpenAICompatProvider::new(&settings);

    // Conditional Windsurf provider initialization
    let windsurf_provider =
        if settings.windsurf_api_key.is_some() || settings.codeium_auth_token.is_some() {
            let auth_source = if settings.windsurf_api_key.is_some() {
                "WINDSURF_API_KEY"
            } else {
                "CODEIUM_AUTH_TOKEN"
            };
            info!(
                "{} detected, initializing Windsurf provider...",
                auth_source
            );
            match providers::WindsurfProvider::new(
                settings.windsurf_api_key.as_deref(),
                settings.codeium_auth_token.as_deref(),
                &settings.windsurf_ls_path,
                settings.windsurf_ls_port,
                &settings.windsurf_api_server_url,
            )
            .await
            {
                Ok(wp) => {
                    info!("Windsurf provider ready");
                    Some(Arc::new(wp))
                }
                Err(e) => {
                    tracing::error!("Failed to initialize Windsurf provider: {}", e);
                    info!("Continuing without Windsurf provider");
                    None
                }
            }
        } else {
            info!("Windsurf provider disabled (WINDSURF_API_KEY and CODEIUM_AUTH_TOKEN not set)");
            None
        };

    let state = Arc::new(AppState {
        settings: settings.clone(),
        provider,
        windsurf_provider,
    });

    let shutdown_state = state.clone();

    let app = Router::new()
        .route("/v1/messages", post(routes::create_message))
        .route("/v1/messages/count_tokens", post(routes::count_tokens))
        .route("/health", get(routes::health))
        .route("/", get(routes::root))
        .with_state(state);

    let addr = format!("{}:{}", settings.host, settings.port);
    info!("Crustoxy starting on {}", addr);
    info!("Default model: {}", settings.model);
    info!(
        "IP rotation: {}",
        if settings.enable_ip_rotation {
            "enabled"
        } else {
            "disabled"
        }
    );

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Received shutdown signal");
            if let Some(ref ws) = shutdown_state.windsurf_provider {
                ws.shutdown().await;
                info!("Windsurf provider shut down");
            }
        })
        .await
        .unwrap();
}
