use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use std::sync::OnceLock;

static IP_ROTATION_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

fn get_lock() -> Arc<Mutex<()>> {
    IP_ROTATION_LOCK
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
}

async fn run_cmd(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to execute {}: {}", cmd, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!("{} failed: {}", cmd, stderr));
    }
    Ok(stdout.trim().to_string())
}

pub async fn rotate_ip() -> Result<(), String> {
    let lock = get_lock();
    let _guard = lock.lock().await;

    info!("Starting IP rotation via WARP...");

    info!("Disconnecting WARP...");
    let _ = run_cmd("warp-cli", &["--accept-tos", "disconnect"]).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    match run_cmd("curl", &["-s", "--max-time", "5", "https://ifconfig.me"]).await {
        Ok(ip) => info!("Current IP before rotation: {}", ip),
        Err(e) => warn!("Could not get current IP: {}", e),
    }

    info!("Deleting WARP registration...");
    let _ = run_cmd("warp-cli", &["--accept-tos", "registration", "delete"]).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    info!("Creating new WARP registration...");
    if let Err(e) = run_cmd("warp-cli", &["--accept-tos", "registration", "new"]).await {
        error!("Failed to create new WARP registration: {}", e);
        return Err(e);
    }

    info!("Connecting WARP...");
    if let Err(e) = run_cmd("warp-cli", &["--accept-tos", "connect"]).await {
        error!("Failed to connect WARP: {}", e);
        return Err(e);
    }

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    match run_cmd("warp-cli", &["--accept-tos", "status"]).await {
        Ok(status) => info!("WARP status: {}", status),
        Err(e) => warn!("Could not get WARP status: {}", e),
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    match run_cmd("curl", &["-s", "--max-time", "5", "https://ifconfig.me"]).await {
        Ok(ip) => info!("New IP after rotation: {}", ip),
        Err(e) => warn!("Could not verify new IP: {}", e),
    }

    info!("IP rotation completed");
    Ok(())
}
