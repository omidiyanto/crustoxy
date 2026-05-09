//! Persistent configuration schema for Crustoxy Panel.
//!
//! Maps 1:1 to the TOML config file at `~/.config/crustoxy/config.toml`.
//! All runtime configuration that was previously in env vars (models, keys,
//! features, routing) now lives here.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Top-level configuration persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelConfig {
    pub general: GeneralConfig,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
}

/// General settings that apply across all profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub active_profile: String,
}

/// A single named profile containing all proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default = "default_profile_name")]
    pub name: String,
    #[serde(default)]
    pub model_mapping: ModelMapping,
    #[serde(default)]
    pub provider_keys: HashMap<String, String>,
    #[serde(default)]
    pub provider_base_urls: HashMap<String, String>,
    #[serde(default)]
    pub features: FeatureFlags,
    #[serde(default)]
    pub rate_limiting: RateLimitConfig,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
}

/// Model mapping per Claude tier. Each value is semicolon-separated for
/// multiple models (auto-routing / load-balancing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMapping {
    #[serde(default = "default_model")]
    pub default: String,
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
}

/// Feature flags controlling proxy optimizations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    #[serde(default = "default_true")]
    pub enable_ip_rotation: bool,
    #[serde(default = "default_true")]
    pub enable_network_probe_mock: bool,
    #[serde(default = "default_true")]
    pub enable_title_generation_skip: bool,
    #[serde(default = "default_true")]
    pub enable_suggestion_mode_skip: bool,
    #[serde(default = "default_true")]
    pub fast_prefix_detection: bool,
    #[serde(default = "default_true")]
    pub enable_filepath_extraction_mock: bool,
    #[serde(default = "default_true")]
    pub enable_tool_retry: bool,
    #[serde(default = "default_tool_retry_max")]
    pub tool_retry_max: u32,
    #[serde(default = "default_true")]
    pub enable_rtk: bool,
    #[serde(default)]
    pub override_system_prompt: Option<String>,
}

/// Rate limiting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_rate_limit")]
    pub provider_rate_limit: u32,
    #[serde(default = "default_rate_window")]
    pub provider_rate_window: u64,
    #[serde(default = "default_max_concurrency")]
    pub provider_max_concurrency: usize,
}

/// HTTP timeout configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    #[serde(default = "default_read_timeout")]
    pub http_read_timeout: u64,
    #[serde(default = "default_connect_timeout")]
    pub http_connect_timeout: u64,
}

/// Routing strategy configuration for model and key selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default = "default_strategy")]
    pub model_strategy: String,
    #[serde(default = "default_strategy")]
    pub key_strategy: String,
    #[serde(default = "default_cooldown")]
    pub rate_limit_cooldown: u64,
    #[serde(default = "default_max_errors")]
    pub max_consecutive_errors: u32,
    #[serde(default = "default_recovery_interval")]
    pub health_recovery_interval: u64,
}

/// Routing strategy enum for type-safe matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingStrategy {
    RoundRobin,
    Random,
    LeastErrors,
}

// ── Default value functions ──────────────────────────────────────────────────

fn default_profile_name() -> String {
    "Default Profile".to_string()
}

fn default_model() -> String {
    "openrouter/meta-llama/llama-3-8b-instruct:free".to_string()
}

fn default_true() -> bool {
    true
}

fn default_tool_retry_max() -> u32 {
    2
}

fn default_rate_limit() -> u32 {
    40
}

fn default_rate_window() -> u64 {
    60
}

fn default_max_concurrency() -> usize {
    5
}

fn default_read_timeout() -> u64 {
    300
}

fn default_connect_timeout() -> u64 {
    10
}

fn default_strategy() -> String {
    "round_robin".to_string()
}

fn default_cooldown() -> u64 {
    60
}

fn default_max_errors() -> u32 {
    3
}

fn default_recovery_interval() -> u64 {
    120
}

// ── Implementations ──────────────────────────────────────────────────────────

impl Default for PanelConfig {
    fn default() -> Self {
        let mut profiles = HashMap::new();
        profiles.insert("default".to_string(), ProfileConfig::default());
        Self {
            general: GeneralConfig {
                active_profile: "default".to_string(),
            },
            profiles,
        }
    }
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            name: default_profile_name(),
            model_mapping: ModelMapping::default(),
            provider_keys: HashMap::new(),
            provider_base_urls: HashMap::new(),
            features: FeatureFlags::default(),
            rate_limiting: RateLimitConfig::default(),
            timeouts: TimeoutConfig::default(),
            routing: RoutingConfig::default(),
        }
    }
}

impl Default for ModelMapping {
    fn default() -> Self {
        Self {
            default: default_model(),
            opus: String::new(),
            sonnet: String::new(),
            haiku: String::new(),
        }
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            enable_ip_rotation: true,
            enable_network_probe_mock: true,
            enable_title_generation_skip: true,
            enable_suggestion_mode_skip: true,
            fast_prefix_detection: true,
            enable_filepath_extraction_mock: true,
            enable_tool_retry: true,
            tool_retry_max: 2,
            enable_rtk: true,
            override_system_prompt: None,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            provider_rate_limit: default_rate_limit(),
            provider_rate_window: default_rate_window(),
            provider_max_concurrency: default_max_concurrency(),
        }
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            http_read_timeout: default_read_timeout(),
            http_connect_timeout: default_connect_timeout(),
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            model_strategy: default_strategy(),
            key_strategy: default_strategy(),
            rate_limit_cooldown: default_cooldown(),
            max_consecutive_errors: default_max_errors(),
            health_recovery_interval: default_recovery_interval(),
        }
    }
}

impl RoutingStrategy {
    /// Parse a strategy string into the enum variant.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "random" => Self::Random,
            "least_errors" => Self::LeastErrors,
            _ => Self::RoundRobin,
        }
    }
}

impl ModelMapping {
    /// Parse a semicolon-separated model list for a given tier.
    #[allow(dead_code)]
    pub fn models_for_tier(&self, tier: &str) -> Vec<String> {
        let raw = match tier {
            "opus" => &self.opus,
            "sonnet" => &self.sonnet,
            "haiku" => &self.haiku,
            _ => &self.default,
        };

        if raw.is_empty() {
            // Fallback to default model list
            return parse_model_list(&self.default);
        }
        parse_model_list(raw)
    }
}

impl ProfileConfig {
    /// Parse semicolon-separated keys for a given provider.
    pub fn keys_for_provider(&self, provider: &str) -> Vec<String> {
        self.provider_keys
            .get(provider)
            .map(|raw| {
                raw.split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl PanelConfig {
    /// Get the currently active profile.
    pub fn active_profile(&self) -> &ProfileConfig {
        self.profiles
            .get(&self.general.active_profile)
            .or_else(|| self.profiles.values().next())
            .expect("config must have at least one profile")
    }

    /// Get a mutable reference to the active profile.
    pub fn active_profile_mut(&mut self) -> &mut ProfileConfig {
        let key = self.general.active_profile.clone();
        self.profiles
            .get_mut(&key)
            .expect("config must have at least one profile")
    }

    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        toml::from_str(&content).map_err(|e| format!("failed to parse config: {e}"))
    }

    /// Save configuration to a TOML file, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create config directory: {e}"))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("failed to serialize config: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("failed to write config: {e}"))
    }

    /// Get the default config file path: `~/.config/crustoxy/config.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("/root/.config"))
            .join("crustoxy")
            .join("config.toml")
    }
}

/// Parse a semicolon-separated model list into individual model strings.
pub fn parse_model_list(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_roundtrip() {
        let config = PanelConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: PanelConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.general.active_profile, "default");
        assert!(parsed.profiles.contains_key("default"));
    }

    #[test]
    fn test_parse_model_list() {
        let models = parse_model_list(
            "nvidia_nim/minimax/minimax-m2.7 ; huggingface/minimax/minimax-m2.7 ; openrouter/moonshotai/kimi-k2.6",
        );
        assert_eq!(models.len(), 3);
        assert_eq!(models[0], "nvidia_nim/minimax/minimax-m2.7");
        assert_eq!(models[2], "openrouter/moonshotai/kimi-k2.6");
    }

    #[test]
    fn test_models_for_tier_fallback() {
        let mapping = ModelMapping {
            default: "openrouter/llama".to_string(),
            opus: String::new(),
            sonnet: "groq/model-a ; groq/model-b".to_string(),
            haiku: String::new(),
        };
        // Empty opus falls back to default
        assert_eq!(mapping.models_for_tier("opus"), vec!["openrouter/llama"]);
        // Sonnet has its own models
        assert_eq!(
            mapping.models_for_tier("sonnet"),
            vec!["groq/model-a", "groq/model-b"]
        );
    }

    #[test]
    fn test_keys_for_provider() {
        let mut profile = ProfileConfig::default();
        profile
            .provider_keys
            .insert("openrouter".to_string(), "key1 ; key2 ; key3".to_string());
        let keys = profile.keys_for_provider("openrouter");
        assert_eq!(keys, vec!["key1", "key2", "key3"]);
        assert!(profile.keys_for_provider("unknown").is_empty());
    }

    #[test]
    fn test_routing_strategy_parse() {
        assert_eq!(
            RoutingStrategy::parse("round_robin"),
            RoutingStrategy::RoundRobin
        );
        assert_eq!(RoutingStrategy::parse("random"), RoutingStrategy::Random);
        assert_eq!(
            RoutingStrategy::parse("least_errors"),
            RoutingStrategy::LeastErrors
        );
        assert_eq!(
            RoutingStrategy::parse("unknown"),
            RoutingStrategy::RoundRobin
        );
    }
}
