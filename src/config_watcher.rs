//! File system watcher for hot-reload of `config.toml`.
//!
//! Watches the config file for changes and broadcasts the new
//! configuration to all listeners via a `tokio::sync::broadcast` channel.

use std::path::PathBuf;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::panel_config::PanelConfig;

/// Spawn a file watcher that monitors `config.toml` for changes.
///
/// When the file is modified, the new config is parsed and broadcast.
/// Returns a `JoinHandle` that runs until the process exits.
pub fn spawn_config_watcher(
    config_path: PathBuf,
    tx: broadcast::Sender<PanelConfig>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<()>(4);

        let watched_path = config_path.clone();
        let watch_dir = config_path.parent().unwrap_or(&config_path).to_path_buf();

        // Create the watcher in a blocking context since notify uses sync callbacks
        let _watcher: RecommendedWatcher = {
            let notify_tx = notify_tx.clone();
            let watched = watched_path.clone();
            match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let dominated =
                        matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
                    if dominated && event.paths.iter().any(|p| p.ends_with("config.toml")) {
                        let _ = notify_tx.blocking_send(());
                    }
                    // Also trigger on the exact path match
                    if dominated && event.paths.contains(&watched) {
                        let _ = notify_tx.blocking_send(());
                    }
                }
            }) {
                Ok(mut w) => {
                    if let Err(e) = w.watch(&watch_dir, RecursiveMode::NonRecursive) {
                        error!("Failed to watch config directory: {}", e);
                        return;
                    }
                    info!("Config watcher active on {}", watch_dir.display());
                    w
                }
                Err(e) => {
                    error!("Failed to create config watcher: {}", e);
                    return;
                }
            }
        };

        // Debounce: wait a short period after a change before reloading
        let debounce_ms = 500;

        loop {
            if notify_rx.recv().await.is_none() {
                info!("Config watcher channel closed, stopping");
                break;
            }

            // Debounce: drain any rapid-fire events
            tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
            while notify_rx.try_recv().is_ok() {}

            // Reload the config
            match PanelConfig::load(&config_path) {
                Ok(new_config) => {
                    info!("Config file changed, broadcasting reload...");
                    if let Err(e) = tx.send(new_config) {
                        warn!("No config reload receivers: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to reload config: {}", e);
                }
            }
        }
    })
}
