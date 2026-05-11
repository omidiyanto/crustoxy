use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;
use uuid::Uuid;

pub const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

pub fn oauth_host() -> String {
    env::var("KIMI_OAUTH_HOST").unwrap_or_else(|_| "https://auth.kimi.com".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAuth {
    pub access: String,
    pub refresh: String,
    pub expires_ms: i64,
    pub scope: Option<String>,
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub scope: Option<String>,
}

pub struct AuthManager {
    cache: RwLock<Option<StoredAuth>>,
    inflight: Mutex<()>,
    client: Client,
}

impl AuthManager {
    pub fn new(client: Client) -> Self {
        Self {
            cache: RwLock::new(None),
            inflight: Mutex::new(()),
            client,
        }
    }

    pub async fn try_load_existing(&self) -> Result<Option<StoredAuth>, String> {
        if let Some(auth) = load_auth()? {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            if auth.expires_ms - REFRESH_MARGIN_MS > now {
                let mut cache = self.cache.write().await;
                *cache = Some(auth.clone());
                return Ok(Some(auth));
            } else {
                // Try refresh
                if let Ok(refreshed) = self.refresh_now(&auth.refresh).await {
                    let mut cache = self.cache.write().await;
                    *cache = Some(refreshed.clone());
                    return Ok(Some(refreshed));
                }
            }
        }
        Ok(None)
    }

    pub async fn persist_initial(&self, tokens: TokenResponse) -> Result<(), String> {
        let auth = StoredAuth {
            access: tokens.access_token,
            refresh: tokens.refresh_token,
            expires_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + (tokens.expires_in as i64 * 1000),
            scope: tokens.scope,
            user_id: None, // Could parse JWT here if needed
        };
        save_auth(&auth)?;
        let mut cache = self.cache.write().await;
        *cache = Some(auth);
        Ok(())
    }

    pub async fn get_auth(&self) -> Result<StoredAuth, String> {
        let cache = self.cache.read().await;
        if let Some(auth) = cache.clone() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            if auth.expires_ms - REFRESH_MARGIN_MS > now {
                return Ok(auth);
            }
        }
        drop(cache);

        let _lock = self.inflight.lock().await;
        // Check again
        let cache = self.cache.read().await;
        if let Some(auth) = cache.clone() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            if auth.expires_ms - REFRESH_MARGIN_MS > now {
                return Ok(auth);
            }
        }
        let refresh_token = cache
            .as_ref()
            .map(|a| a.refresh.clone())
            .unwrap_or_default();
        drop(cache);

        if refresh_token.is_empty() {
            return Err("No auth token available, restart to login".to_string());
        }

        let refreshed = self.refresh_now(&refresh_token).await?;
        let mut cache = self.cache.write().await;
        *cache = Some(refreshed.clone());
        Ok(refreshed)
    }

    pub async fn force_refresh(&self) -> Result<StoredAuth, String> {
        let _lock = self.inflight.lock().await;
        let cache = self.cache.read().await;
        let refresh_token = cache
            .as_ref()
            .map(|a| a.refresh.clone())
            .unwrap_or_default();
        drop(cache);

        if refresh_token.is_empty() {
            return Err("No auth token available, restart to login".to_string());
        }

        let refreshed = self.refresh_now(&refresh_token).await?;
        let mut cache = self.cache.write().await;
        *cache = Some(refreshed.clone());
        Ok(refreshed)
    }

    async fn refresh_now(&self, refresh_token: &str) -> Result<StoredAuth, String> {
        let resp = self
            .client
            .post(format!("{}/api/oauth/token", oauth_host()))
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| format!("Refresh failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Refresh rejected: {}", resp.status()));
        }

        let tokens: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Parse error: {}", e))?;

        let auth = StoredAuth {
            access: tokens.access_token,
            refresh: tokens.refresh_token,
            expires_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + (tokens.expires_in as i64 * 1000),
            scope: tokens.scope,
            user_id: None,
        };
        save_auth(&auth)?;
        Ok(auth)
    }
}

pub async fn run_device_login(client: &Client) -> Result<TokenResponse, String> {
    let resp = client
        .post(format!("{}/api/oauth/device_authorization", oauth_host()))
        .form(&[("client_id", CLIENT_ID)])
        .send()
        .await
        .map_err(|e| format!("Device auth request failed: {}", e))?;

    if !resp.status().is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!("Device auth rejected: {}", err_body));
    }

    let auth_info: DeviceAuthResponse = resp
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    println!("\n================ KIMI OAUTH LOGIN ================");
    println!("Please visit: {}", auth_info.verification_uri_complete);
    println!("Your code:    {}", auth_info.user_code);
    println!("Waiting for authorization...\n");

    let mut attempts = 0;
    let max_attempts = auth_info.expires_in / auth_info.interval;

    while attempts < max_attempts {
        tokio::time::sleep(tokio::time::Duration::from_secs(auth_info.interval)).await;
        attempts += 1;

        let token_resp = client
            .post(format!("{}/api/oauth/token", oauth_host()))
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &auth_info.device_code),
            ])
            .send()
            .await;

        if let Ok(res) = token_resp {
            if res.status().is_success() {
                let tokens: TokenResponse = res
                    .json()
                    .await
                    .map_err(|e| format!("Parse error: {}", e))?;
                return Ok(tokens);
            } else {
                let err_body = res.text().await.unwrap_or_default();
                if !err_body.contains("authorization_pending") {
                    warn!("Login polling non-fatal error: {}", err_body);
                }
            }
        }
    }

    Err("Login timed out".to_string())
}

fn auth_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("crustoxy")
        .join("kimi_oauth")
        .join("auth.json")
}

fn load_auth() -> Result<Option<StoredAuth>, String> {
    let path = auth_file_path();
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let auth = serde_json::from_str(&data).map_err(|e| format!("Parse error: {}", e))?;
    Ok(Some(auth))
}

fn save_auth(auth: &StoredAuth) -> Result<(), String> {
    let path = auth_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Create dir error: {}", e))?;
    }
    let data = serde_json::to_string_pretty(auth).map_err(|e| format!("Serialize error: {}", e))?;

    // Attempt secure write on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("Open error: {}", e))?;
        use std::io::Write;
        file.write_all(data.as_bytes())
            .map_err(|e| format!("Write error: {}", e))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, data).map_err(|e| format!("Write error: {}", e))?;
    }

    Ok(())
}

fn get_or_create_device_id() -> String {
    let path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("crustoxy")
        .join("kimi_oauth")
        .join("device_id");
    if path.exists()
        && let Ok(id) = std::fs::read_to_string(&path)
    {
        return id.trim().to_string();
    }
    let new_id = Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, &new_id);
    new_id
}

pub fn build_common_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    let device_id = get_or_create_device_id();
    let device_name = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    headers.insert(
        HeaderName::from_static("x-msh-platform"),
        HeaderValue::from_static("kimi_cli"),
    );
    headers.insert(
        HeaderName::from_static("x-msh-version"),
        HeaderValue::from_static("1.37.0"),
    );
    if let Ok(val) = HeaderValue::from_str(&device_id) {
        headers.insert(HeaderName::from_static("x-msh-device-id"), val);
    }
    if let Ok(val) = HeaderValue::from_str(&device_name) {
        headers.insert(HeaderName::from_static("x-msh-device-name"), val);
    }
    headers.insert(
        HeaderName::from_static("x-msh-os-version"),
        HeaderValue::from_static("linux"),
    );
    headers.insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static("KimiCLI/1.37.0"),
    );
    headers
}
