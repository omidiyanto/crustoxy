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

    // Conditional Puter provider initialization
    let puter_provider = if let Some(ref creds) = settings.puter_api_key {
        info!("PUTER_API_KEY detected, initializing Puter provider...");
        match providers::PuterProvider::new(creds, &settings).await {
            Ok(pp) => {
                info!("Puter provider ready");
                Some(Arc::new(pp))
            }
            Err(e) => {
                tracing::error!("Failed to initialize Puter provider: {}", e);
                info!("Continuing without Puter provider");
                None
            }
        }
    } else {
        None
    };

    let kimi_oauth_provider = if settings.kimi_oauth_enable {
        info!("KIMI_OAUTH_ENABLE detected, checking provider initialization...");
        match providers::kimi_oauth::bootstrap_if_enabled(&settings).await {
            Ok(Some(provider)) => {
                info!("Kimi OAuth provider ready");
                Some(provider)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!("Failed to initialize Kimi OAuth provider: {}", e);
                info!("Continuing without Kimi OAuth provider");
                None
            }
        }
    } else {
        None
    };

    let cloudflare_provider = if settings.cloudflare_api_key.is_some() {
        info!("CLOUDFLARE_API_KEY detected, initializing Cloudflare provider...");
        Some(Arc::new(providers::CloudflareProvider::new(&settings)))
    } else {
        None
    };

    let state = Arc::new(AppState {
        settings: settings.clone(),
        provider,
        puter_provider,
        kimi_oauth_provider,
        cloudflare_provider,
    });

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
        })
        .await
        .unwrap();
}
