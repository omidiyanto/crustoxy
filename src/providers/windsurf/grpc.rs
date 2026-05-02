//! gRPC client for the local Windsurf language server binary.
//! Uses HTTP/2 cleartext (h2c) via the `h2` crate.
//!
//! Ported from WindsurfAPI/src/grpc.js

use bytes::Bytes;
use h2::client::SendRequest;
use http::Request;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, warn};

const DEFAULT_CSRF: &str = "windsurf-api-csrf-fixed-token";

/// Wrap a protobuf payload in a gRPC frame (5-byte header).
pub fn grpc_frame(payload: &[u8]) -> Bytes {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0); // compression flag = uncompressed
    frame.extend(&(payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    Bytes::from(frame)
}

/// Strip gRPC frame header (5 bytes) from a response buffer.
pub fn strip_grpc_frame(buf: &[u8]) -> &[u8] {
    if buf.len() >= 5 && buf[0] == 0 {
        let msg_len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() >= 5 + msg_len {
            return &buf[5..5 + msg_len];
        }
    }
    buf
}

/// Extract all gRPC frames from a buffer (may contain multiple concatenated frames).
pub fn extract_grpc_frames(buf: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + 5 <= buf.len() {
        let compressed = buf[offset];
        let msg_len = u32::from_be_bytes([
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
            buf[offset + 4],
        ]) as usize;
        if compressed != 0 || offset + 5 + msg_len > buf.len() {
            break;
        }
        frames.push(buf[offset + 5..offset + 5 + msg_len].to_vec());
        offset += 5 + msg_len;
    }
    frames
}

/// Persistent HTTP/2 session to the language server.
pub struct GrpcSession {
    sender: Arc<Mutex<Option<SendRequest<Bytes>>>>,
    port: u16,
}

impl GrpcSession {
    pub fn new(port: u16) -> Self {
        Self {
            sender: Arc::new(Mutex::new(None)),
            port,
        }
    }

    /// Get or create an HTTP/2 connection to the language server.
    async fn get_sender(&self) -> Result<SendRequest<Bytes>, String> {
        let mut guard = self.sender.lock().await;

        // Reuse existing connection if still alive
        if let Some(ref sender) = *guard {
            // h2 sender clone is cheap — it's a handle to the shared session
            return Ok(sender.clone());
        }

        // Create new h2c connection
        let addr = format!("127.0.0.1:{}", self.port);
        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("Failed to connect to LS at {}: {}", addr, e))?;

        let (sender, conn) = h2::client::handshake(tcp)
            .await
            .map_err(|e| format!("H2 handshake failed: {}", e))?;

        // Spawn connection driver
        let port = self.port;
        let sender_ref = self.sender.clone();
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!("Windsurf H2 connection to port {} closed: {}", port, e);
            }
            // Clear the cached sender so next call reconnects
            let mut guard = sender_ref.lock().await;
            *guard = None;
        });

        *guard = Some(sender.clone());
        debug!("Windsurf gRPC: new H2 session to port {}", self.port);
        Ok(sender)
    }

    /// Close the cached session (e.g. when LS restarts).
    pub async fn close(&self) {
        let mut guard = self.sender.lock().await;
        *guard = None;
    }

    /// Make a unary gRPC call to the language server.
    pub async fn unary(
        &self,
        path: &str,
        body: &[u8],
        csrf_token: &str,
        timeout_ms: u64,
    ) -> Result<Vec<u8>, String> {
        let mut sender = self.get_sender().await?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{}{}", self.port, path))
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .header("user-agent", "grpc-rust/1.0.0")
            .header("x-codeium-csrf-token", csrf_token)
            .body(())
            .map_err(|e| format!("Failed to build request: {}", e))?;

        let (response, mut send_stream) = sender
            .send_request(request, false)
            .map_err(|e| format!("Failed to send request: {}", e))?;

        // Send grpc-framed body
        send_stream
            .send_data(grpc_frame(body), true)
            .map_err(|e| format!("Failed to send data: {}", e))?;

        // Wait for response with timeout
        let response = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), response)
            .await
            .map_err(|_| "gRPC unary timeout".to_string())?
            .map_err(|e| format!("gRPC response error: {}", e))?;

        // Read response body
        let mut body_data = Vec::new();
        let mut recv_stream = response.into_body();
        while let Some(chunk) = recv_stream.data().await {
            let chunk = chunk.map_err(|e| format!("gRPC body read error: {}", e))?;
            body_data.extend(&chunk);
            // Release flow control
            let _ = recv_stream.flow_control().release_capacity(chunk.len());
        }

        // Check trailers for gRPC status
        if let Some(trailers) = recv_stream
            .trailers()
            .await
            .map_err(|e| format!("gRPC trailers error: {}", e))?
        {
            let status = trailers
                .get("grpc-status")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("0");
            if status != "0" {
                let message = trailers
                    .get("grpc-message")
                    .and_then(|v| v.to_str().ok())
                    .map(urlencoding_decode)
                    .unwrap_or_else(|| format!("gRPC status {}", status));
                return Err(message);
            }
        }

        // Strip gRPC frame headers
        let frames = extract_grpc_frames(&body_data);
        if !frames.is_empty() {
            Ok(frames.into_iter().flatten().collect())
        } else {
            Ok(strip_grpc_frame(&body_data).to_vec())
        }
    }

    /// Make a streaming gRPC call. Returns chunks as they arrive via a channel.
    pub async fn stream(
        &self,
        path: &str,
        body: &[u8],
        csrf_token: &str,
        timeout_ms: u64,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<Vec<u8>, String>>, String> {
        let mut sender = self.get_sender().await?;

        let request = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{}{}", self.port, path))
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .header("grpc-accept-encoding", "identity")
            .header("user-agent", "grpc-rust/1.0.0")
            .header("x-codeium-csrf-token", csrf_token)
            .body(())
            .map_err(|e| format!("Failed to build request: {}", e))?;

        let (response, mut send_stream) = sender
            .send_request(request, false)
            .map_err(|e| format!("Failed to send request: {}", e))?;

        send_stream
            .send_data(grpc_frame(body), true)
            .map_err(|e| format!("Failed to send data: {}", e))?;

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            let response =
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), response)
                    .await
                {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        let _ = tx.send(Err(format!("gRPC stream error: {}", e))).await;
                        return;
                    }
                    Err(_) => {
                        let _ = tx.send(Err("gRPC stream timeout".to_string())).await;
                        return;
                    }
                };

            let mut recv_stream = response.into_body();
            let mut pending = Vec::new();

            while let Some(chunk) = recv_stream.data().await {
                match chunk {
                    Ok(data) => {
                        let _ = recv_stream.flow_control().release_capacity(data.len());
                        pending.extend(&data);

                        // Parse gRPC frames from pending buffer
                        while pending.len() >= 5 {
                            let compressed = pending[0];
                            let msg_len = u32::from_be_bytes([
                                pending[1], pending[2], pending[3], pending[4],
                            ]) as usize;
                            if pending.len() < 5 + msg_len {
                                break;
                            }
                            if compressed == 0 {
                                let payload = pending[5..5 + msg_len].to_vec();
                                if tx.send(Ok(payload)).await.is_err() {
                                    return;
                                }
                            }
                            pending = pending[5 + msg_len..].to_vec();
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("gRPC data error: {}", e))).await;
                        return;
                    }
                }
            }

            // Check trailers
            if let Ok(Some(trailers)) = recv_stream.trailers().await {
                let status = trailers
                    .get("grpc-status")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("0");
                if status != "0" {
                    let message = trailers
                        .get("grpc-message")
                        .and_then(|v| v.to_str().ok())
                        .map(urlencoding_decode)
                        .unwrap_or_else(|| format!("gRPC status {}", status));
                    let _ = tx.send(Err(message)).await;
                }
            }
        });

        Ok(rx)
    }
}

/// Simple percent-decoding for gRPC error messages.
fn urlencoding_decode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next().unwrap_or(b'0');
            let h2 = chars.next().unwrap_or(b'0');
            let hex = format!("{}{}", h1 as char, h2 as char);
            if let Ok(decoded) = u8::from_str_radix(&hex, 16) {
                result.push(decoded as char);
            } else {
                result.push('%');
                result.push(h1 as char);
                result.push(h2 as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

/// Default CSRF token for the language server.
pub fn default_csrf_token() -> &'static str {
    DEFAULT_CSRF
}
