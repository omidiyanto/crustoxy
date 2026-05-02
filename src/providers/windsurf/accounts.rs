//! Account persistence for the Windsurf provider.
//!
//! Mirrors WindsurfAPI/src/auth.js account pool — persists API keys to
//! accounts.json so CODEIUM_AUTH_TOKEN only needs to be exchanged once.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// A persisted Windsurf account entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(rename = "apiServerUrl")]
    pub api_server_url: String,
    pub method: String,
    pub status: String,
    #[serde(rename = "addedAt")]
    pub added_at: u64,
    pub tier: String,
    /// The original auth token used to register (for matching on restart)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token_hash: Option<String>,
}

/// Simple hash for matching tokens without storing them in plaintext.
fn hash_token(token: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Get the accounts.json file path for the given data directory.
fn accounts_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("accounts.json")
}

/// Load accounts from disk. Returns empty vec if file doesn't exist or is invalid.
pub fn load_accounts(data_dir: &str) -> Vec<Account> {
    let path = accounts_path(data_dir);
    if !path.exists() {
        debug!("No accounts.json found at {}", path.display());
        return Vec::new();
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<Vec<Account>>(&content) {
            Ok(accounts) => {
                info!(
                    "Loaded {} account(s) from {}",
                    accounts.len(),
                    path.display()
                );
                accounts
            }
            Err(e) => {
                warn!("Failed to parse {}: {}", path.display(), e);
                Vec::new()
            }
        },
        Err(e) => {
            warn!("Failed to read {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

/// Save accounts to disk atomically (write to .tmp then rename).
pub fn save_accounts(data_dir: &str, accounts: &[Account]) {
    let path = accounts_path(data_dir);
    let tmp_path = path.with_extension("json.tmp");

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(accounts) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&tmp_path, &json) {
                warn!("Failed to write {}: {}", tmp_path.display(), e);
                return;
            }
            if let Err(e) = std::fs::rename(&tmp_path, &path) {
                warn!(
                    "Failed to rename {} → {}: {}",
                    tmp_path.display(),
                    path.display(),
                    e
                );
                // Fallback: try direct write
                std::fs::write(&path, &json).ok();
            }
            debug!("Saved {} account(s) to {}", accounts.len(), path.display());
        }
        Err(e) => {
            warn!("Failed to serialize accounts: {}", e);
        }
    }
}

/// Find an existing account that was created from the same auth token.
pub fn find_account_by_token<'a>(accounts: &'a [Account], token: &str) -> Option<&'a Account> {
    let h = hash_token(token);
    accounts
        .iter()
        .find(|a| a.auth_token_hash.as_deref() == Some(&h) && a.status == "active")
}

/// Create a new Account entry after successful token exchange.
pub fn create_account(api_key: &str, api_server_url: &str, auth_token: &str) -> Account {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Account {
        id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
        email: format!("token-{}", &hash_token(auth_token)[..8]),
        api_key: api_key.to_string(),
        api_server_url: api_server_url.to_string(),
        method: "token".to_string(),
        status: "active".to_string(),
        added_at: now,
        tier: "unknown".to_string(),
        auth_token_hash: Some(hash_token(auth_token)),
    }
}
