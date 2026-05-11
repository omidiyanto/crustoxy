//! Configuration loader with automatic migration from legacy env vars.
//!
//! Handles the full lifecycle:
//! 1. Check for existing `config.toml` → load it
//! 2. If missing, check for legacy `.env` vars → migrate → save
//! 3. If no legacy vars either → return default (setup mode)

use std::env;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::panel_config::{
    FeatureFlags, ModelMapping, PanelConfig, RateLimitConfig, RoutingConfig, TimeoutConfig,
};

/// Result of config loading — distinguishes between existing, migrated, and fresh configs.
#[derive(Debug)]
pub enum ConfigLoadResult {
    /// Config loaded from existing file.
    Loaded(PanelConfig),
    /// Config migrated from legacy env vars and saved.
    Migrated(PanelConfig),
    /// No config found — fresh default for setup mode.
    Fresh(PanelConfig),
}

impl ConfigLoadResult {
    /// Extract the config regardless of how it was obtained.
    pub fn into_config(self) -> PanelConfig {
        match self {
            Self::Loaded(c) | Self::Migrated(c) | Self::Fresh(c) => c,
        }
    }

    /// Whether the proxy has a usable configuration (not fresh/setup mode).
    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::Fresh(_))
    }
}

/// Load or create the panel configuration.
///
/// - `config_path`: path to `config.toml` (typically `~/.config/crustoxy/config.toml`)
pub fn load_or_create(config_path: &Path) -> ConfigLoadResult {
    // 1. Try loading existing config
    if config_path.exists() {
        match PanelConfig::load(config_path) {
            Ok(config) => {
                info!("Loaded config from {}", config_path.display());
                return ConfigLoadResult::Loaded(config);
            }
            Err(e) => {
                tracing::error!("Failed to parse config at {}: {}", config_path.display(), e);
                tracing::error!("Falling back to migration/defaults");
            }
        }
    }

    // 2. Check for legacy env vars and migrate
    if has_legacy_env_vars() {
        info!("Legacy env vars detected, migrating to config.toml...");
        let config = migrate_from_env();
        if let Err(e) = config.save(config_path) {
            tracing::error!("Failed to save migrated config: {}", e);
        } else {
            info!("Config migrated and saved to {}", config_path.display());
        }
        return ConfigLoadResult::Migrated(config);
    }

    // 3. Fresh start — setup mode
    info!("No configuration found. Starting in setup mode.");
    ConfigLoadResult::Fresh(PanelConfig::default())
}

/// Check if legacy env vars are present (any model or provider key).
fn has_legacy_env_vars() -> bool {
    let model_keys = ["MODEL", "MODEL_OPUS", "MODEL_SONNET", "MODEL_HAIKU"];
    model_keys
        .iter()
        .chain(key_mappings().iter().map(|(_, key)| key))
        .chain(url_mappings().iter().map(|(_, key)| key))
        .chain(feature_env_keys().iter())
        .any(|k| env::var(k).ok().is_some_and(|v| !v.is_empty()))
}

fn key_mappings() -> &'static [(&'static str, &'static str)] {
    &[
        ("openai", "OPENAI_API_KEY"),
        ("sumopod", "SUMOPOD_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("groq", "GROQ_API_KEY"),
        ("deepseek", "DEEPSEEK_API_KEY"),
        ("gemini", "GEMINI_API_KEY"),
        ("together", "TOGETHER_API_KEY"),
        ("huggingface", "HUGGINGFACE_API_KEY"),
        ("mistral", "MISTRAL_API_KEY"),
        ("perplexity", "PERPLEXITY_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
        ("deepinfra", "DEEPINFRA_API_KEY"),
        ("kimi", "KIMI_API_KEY"),
        ("zhipu", "ZHIPU_API_KEY"),
        ("anyscale", "ANYSCALE_API_KEY"),
        ("siliconflow", "SILICONFLOW_API_KEY"),
        ("novita", "NOVITA_API_KEY"),
        ("nvidia_nim", "NVIDIA_NIM_API_KEY"),
        ("modal", "MODAL_API_KEY"),
        ("opencode_zen", "OPENCODE_ZEN_API_KEY"),
        ("cloudflare", "CLOUDFLARE_API_KEY"),
        ("kimi_oauth", "KIMI_OAUTH_API_KEY"),
        ("puter", "PUTER_API_KEY"),
        ("custom", "CUSTOM_API_KEY"),
    ]
}

fn url_mappings() -> &'static [(&'static str, &'static str)] {
    &[
        ("openai", "OPENAI_BASE_URL"),
        ("sumopod", "SUMOPOD_BASE_URL"),
        ("openrouter", "OPENROUTER_BASE_URL"),
        ("groq", "GROQ_BASE_URL"),
        ("deepseek", "DEEPSEEK_BASE_URL"),
        ("gemini", "GEMINI_BASE_URL"),
        ("together", "TOGETHER_BASE_URL"),
        ("huggingface", "HUGGINGFACE_BASE_URL"),
        ("mistral", "MISTRAL_BASE_URL"),
        ("perplexity", "PERPLEXITY_BASE_URL"),
        ("fireworks", "FIREWORKS_BASE_URL"),
        ("deepinfra", "DEEPINFRA_BASE_URL"),
        ("kimi", "KIMI_BASE_URL"),
        ("zhipu", "ZHIPU_BASE_URL"),
        ("anyscale", "ANYSCALE_BASE_URL"),
        ("siliconflow", "SILICONFLOW_BASE_URL"),
        ("novita", "NOVITA_BASE_URL"),
        ("nvidia_nim", "NVIDIA_NIM_BASE_URL"),
        ("modal", "MODAL_BASE_URL"),
        ("opencode_zen", "OPENCODE_ZEN_BASE_URL"),
        ("cloudflare", "CLOUDFLARE_BASE_URL"),
        ("ollama", "OLLAMA_BASE_URL"),
        ("lmstudio", "LMSTUDIO_BASE_URL"),
        ("vllm", "VLLM_BASE_URL"),
        ("llamacpp", "LLAMACPP_BASE_URL"),
        ("custom", "CUSTOM_BASE_URL"),
        ("kimi_oauth", "KIMI_OAUTH_BASE_URL"),
    ]
}

fn feature_env_keys() -> &'static [&'static str] {
    &[
        "ENABLE_IP_ROTATION",
        "ENABLE_NETWORK_PROBE_MOCK",
        "ENABLE_TITLE_GENERATION_SKIP",
        "ENABLE_SUGGESTION_MODE_SKIP",
        "FAST_PREFIX_DETECTION",
        "ENABLE_FILEPATH_EXTRACTION_MOCK",
        "ENABLE_TOOL_RETRY",
        "TOOL_RETRY_MAX",
        "ENABLE_RTK",
        "OVERRIDE_SYSTEM_PROMPT",
        "PROVIDER_RATE_LIMIT",
        "PROVIDER_RATE_WINDOW",
        "PROVIDER_MAX_CONCURRENCY",
        "HTTP_READ_TIMEOUT",
        "HTTP_CONNECT_TIMEOUT",
    ]
}

/// Migrate legacy env vars into a `PanelConfig`.
fn migrate_from_env() -> PanelConfig {
    let mut config = PanelConfig::default();
    let profile = config.active_profile_mut();

    // Model mapping
    profile.model_mapping = ModelMapping {
        default: env_or("MODEL", "openrouter/meta-llama/llama-3-8b-instruct:free"),
        opus: env_or_empty("MODEL_OPUS"),
        sonnet: env_or_empty("MODEL_SONNET"),
        haiku: env_or_empty("MODEL_HAIKU"),
    };

    // Provider keys — migrate all known providers
    for (provider, env_key) in key_mappings() {
        if let Some(key) = env_or_none(env_key) {
            profile.provider_keys.insert((*provider).to_string(), key);
        }
    }

    // Provider base URL overrides
    for (provider, env_key) in url_mappings() {
        if let Some(url) = env_or_none(env_key) {
            profile
                .provider_base_urls
                .insert((*provider).to_string(), url);
        }
    }

    // Feature flags
    profile.features = FeatureFlags {
        enable_ip_rotation: env_bool("ENABLE_IP_ROTATION", false),
        enable_network_probe_mock: env_bool("ENABLE_NETWORK_PROBE_MOCK", true),
        enable_title_generation_skip: env_bool("ENABLE_TITLE_GENERATION_SKIP", true),
        enable_suggestion_mode_skip: env_bool("ENABLE_SUGGESTION_MODE_SKIP", true),
        fast_prefix_detection: env_bool("FAST_PREFIX_DETECTION", true),
        enable_filepath_extraction_mock: env_bool("ENABLE_FILEPATH_EXTRACTION_MOCK", true),
        enable_tool_retry: env_bool("ENABLE_TOOL_RETRY", true),
        tool_retry_max: env_u32("TOOL_RETRY_MAX", 2),
        enable_rtk: env_bool("ENABLE_RTK", true),
        override_system_prompt: env_or_none("OVERRIDE_SYSTEM_PROMPT"),
    };

    // Rate limiting
    profile.rate_limiting = RateLimitConfig {
        provider_rate_limit: env_u32("PROVIDER_RATE_LIMIT", 40),
        provider_rate_window: env_u64("PROVIDER_RATE_WINDOW", 60),
        provider_max_concurrency: env_usize("PROVIDER_MAX_CONCURRENCY", 5),
    };

    // Timeouts
    profile.timeouts = TimeoutConfig {
        http_read_timeout: env_u64("HTTP_READ_TIMEOUT", 300),
        http_connect_timeout: env_u64("HTTP_CONNECT_TIMEOUT", 10),
    };

    // Routing defaults
    profile.routing = RoutingConfig::default();

    config
}

/// Get the config file path, checking env override or using default.
pub fn config_path() -> PathBuf {
    env::var("CRUSTOXY_CONFIG")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(PanelConfig::default_path)
}

// ── Env helper functions ─────────────────────────────────────────────────────

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_or_empty(key: &str) -> String {
    env::var(key).unwrap_or_default()
}

fn env_or_none(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_config_is_not_configured() {
        let result = ConfigLoadResult::Fresh(PanelConfig::default());
        assert!(!result.is_configured());
    }

    #[test]
    fn test_loaded_config_is_configured() {
        let result = ConfigLoadResult::Loaded(PanelConfig::default());
        assert!(result.is_configured());
    }

    #[test]
    fn test_into_config() {
        let config = PanelConfig::default();
        let result = ConfigLoadResult::Fresh(config);
        let extracted = result.into_config();
        assert_eq!(extracted.general.active_profile, "default");
    }
}
