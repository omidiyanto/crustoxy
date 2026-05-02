//! Language server lifecycle manager.
//! Spawns and manages the Windsurf language server binary.
//!
//! Ported from WindsurfAPI/src/langserver.js

use std::path::Path;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::time::{Duration, sleep, timeout};
use tracing::{debug, info, warn};

use super::grpc::GrpcSession;

/// Manages the lifecycle of a Windsurf language server process.
pub struct LanguageServer {
    process: Option<Child>,
    pub port: u16,
    pub csrf_token: String,
    pub session: GrpcSession,
    api_server_url: String,
    binary_path: String,
    ready: bool,
}

impl LanguageServer {
    /// Start the language server binary.
    pub async fn start(
        binary_path: &str,
        port: u16,
        csrf_token: &str,
        api_server_url: &str,
    ) -> Result<Self, String> {
        // Verify binary exists
        if !Path::new(binary_path).exists() {
            return Err(format!(
                "Language server binary not found at {}. \
                 Install it with: bash install-ls.sh (or set WINDSURF_LS_PATH env var)",
                binary_path
            ));
        }

        // Create data directories
        let data_dir = "/opt/windsurf/data";
        let db_dir = format!("{}/db", data_dir);
        std::fs::create_dir_all(&db_dir).ok();

        let args = vec![
            format!("--api_server_url={}", api_server_url),
            format!("--server_port={}", port),
            format!("--csrf_token={}", csrf_token),
            "--register_user_url=https://api.codeium.com/register_user/".to_string(),
            format!("--codeium_dir={}", data_dir),
            format!("--database_dir={}", db_dir),
            "--detect_proxy=false".to_string(),
        ];

        info!(
            "Starting Windsurf LS: {} port={} api={}",
            binary_path, port, api_server_url
        );

        let child = Command::new(binary_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    format!(
                        "LS binary is not executable: {}. Run: chmod +x {}",
                        e, binary_path
                    )
                } else {
                    format!("Failed to spawn LS: {}", e)
                }
            })?;

        let pid = child.id().unwrap_or(0);
        info!("Windsurf LS spawned: pid={} port={}", pid, port);

        // Spawn stdout/stderr loggers
        // (child owns the handles, we read them in background tasks)

        let mut ls = Self {
            process: Some(child),
            port,
            csrf_token: csrf_token.to_string(),
            session: GrpcSession::new(port),
            api_server_url: api_server_url.to_string(),
            binary_path: binary_path.to_string(),
            ready: false,
        };

        // Wait for LS to become ready
        ls.wait_ready(25000).await?;

        Ok(ls)
    }

    /// Wait for the language server to accept HTTP/2 connections.
    async fn wait_ready(&mut self, timeout_ms: u64) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

        while std::time::Instant::now() < deadline {
            match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", self.port)).await {
                Ok(_) => {
                    // Port is open, try a quick H2 handshake
                    match self.probe_h2().await {
                        Ok(true) => {
                            self.ready = true;
                            info!("Windsurf LS ready on port {}", self.port);
                            return Ok(());
                        }
                        Ok(false) => {
                            debug!("LS port {} open but not ready yet", self.port);
                        }
                        Err(e) => {
                            debug!("LS probe error: {}", e);
                        }
                    }
                }
                Err(_) => {
                    debug!("LS port {} not yet open", self.port);
                }
            }
            sleep(Duration::from_millis(500)).await;
        }

        Err(format!(
            "LS port {} not ready after {}ms",
            self.port, timeout_ms
        ))
    }

    /// Quick H2 handshake probe.
    async fn probe_h2(&self) -> Result<bool, String> {
        let addr = format!("127.0.0.1:{}", self.port);
        let tcp = match timeout(
            Duration::from_millis(2000),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(s)) => s,
            _ => return Ok(false),
        };

        match timeout(Duration::from_millis(2000), h2::client::handshake(tcp)).await {
            Ok(Ok((_sender, conn))) => {
                // Connection established, spawn and drop immediately
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Check if the process is still running.
    pub fn is_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(None) => true, // still running
                Ok(Some(status)) => {
                    warn!("Windsurf LS exited with status: {}", status);
                    self.ready = false;
                    false
                }
                Err(e) => {
                    warn!("Failed to check LS status: {}", e);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Stop the language server.
    pub async fn stop(&mut self) {
        if let Some(ref mut child) = self.process {
            info!("Stopping Windsurf LS on port {}", self.port);
            let _ = child.kill().await;
        }
        self.session.close().await;
        self.ready = false;
        self.process = None;
    }

    /// Restart the language server.
    pub async fn restart(&mut self) -> Result<(), String> {
        self.stop().await;
        sleep(Duration::from_millis(500)).await;

        let mut new_ls = Self::start(
            &self.binary_path,
            self.port,
            &self.csrf_token,
            &self.api_server_url,
        )
        .await?;

        // Swap fields using std::mem::take/replace to avoid moving out of Drop
        self.process = std::mem::take(&mut new_ls.process);
        self.session = GrpcSession::new(self.port); // recreate session
        self.ready = new_ls.ready;
        // Drop new_ls shell (process already taken)
        Ok(())
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.process {
            // Best-effort kill — can't await in Drop
            let _ = child.start_kill();
        }
    }
}
