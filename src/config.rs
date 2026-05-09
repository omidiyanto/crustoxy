use std::env;

use crate::panel_config::{PanelConfig, ProfileConfig};

#[derive(Clone, Debug)]
pub struct ProviderDef {
    pub name: &'static str,
    pub env_prefix: &'static str,
    pub default_base_url: &'static str,
}

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        name: "openai",
        env_prefix: "OPENAI",
        default_base_url: "https://api.openai.com/v1",
    },
    ProviderDef {
        name: "openrouter",
        env_prefix: "OPENROUTER",
        default_base_url: "https://openrouter.ai/api/v1",
    },
    ProviderDef {
        name: "groq",
        env_prefix: "GROQ",
        default_base_url: "https://api.groq.com/openai/v1",
    },
    ProviderDef {
        name: "deepseek",
        env_prefix: "DEEPSEEK",
        default_base_url: "https://api.deepseek.com/v1",
    },
    ProviderDef {
        name: "gemini",
        env_prefix: "GEMINI",
        default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
    },
    ProviderDef {
        name: "together",
        env_prefix: "TOGETHER",
        default_base_url: "https://api.together.xyz/v1",
    },
    ProviderDef {
        name: "huggingface",
        env_prefix: "HUGGINGFACE",
        default_base_url: "https://router.huggingface.co/v1",
    },
    ProviderDef {
        name: "mistral",
        env_prefix: "MISTRAL",
        default_base_url: "https://api.mistral.ai/v1",
    },
    ProviderDef {
        name: "perplexity",
        env_prefix: "PERPLEXITY",
        default_base_url: "https://api.perplexity.ai",
    },
    ProviderDef {
        name: "fireworks",
        env_prefix: "FIREWORKS",
        default_base_url: "https://api.fireworks.ai/inference/v1",
    },
    ProviderDef {
        name: "deepinfra",
        env_prefix: "DEEPINFRA",
        default_base_url: "https://api.deepinfra.com/v1/openai",
    },
    ProviderDef {
        name: "kimi",
        env_prefix: "KIMI",
        default_base_url: "https://api.moonshot.cn/v1",
    },
    ProviderDef {
        name: "zhipu",
        env_prefix: "ZHIPU",
        default_base_url: "https://open.bigmodel.cn/api/paas/v4",
    },
    ProviderDef {
        name: "anyscale",
        env_prefix: "ANYSCALE",
        default_base_url: "https://api.endpoints.anyscale.com/v1",
    },
    ProviderDef {
        name: "siliconflow",
        env_prefix: "SILICONFLOW",
        default_base_url: "https://api.siliconflow.com/v1",
    },
    ProviderDef {
        name: "novita",
        env_prefix: "NOVITA",
        default_base_url: "https://api.novita.ai/openai",
    },
    ProviderDef {
        name: "nvidia_nim",
        env_prefix: "NVIDIA_NIM",
        default_base_url: "https://integrate.api.nvidia.com/v1",
    },
    ProviderDef {
        name: "modal",
        env_prefix: "MODAL",
        default_base_url: "https://api.modal.com/v1",
    },
    ProviderDef {
        name: "opencode_zen",
        env_prefix: "OPENCODE_ZEN",
        default_base_url: "https://opencode.ai/zen/v1",
    },
    ProviderDef {
        name: "ollama",
        env_prefix: "OLLAMA",
        default_base_url: "http://localhost:11434/v1",
    },
    ProviderDef {
        name: "lmstudio",
        env_prefix: "LMSTUDIO",
        default_base_url: "http://localhost:1234/v1",
    },
    ProviderDef {
        name: "vllm",
        env_prefix: "VLLM",
        default_base_url: "http://localhost:8000/v1",
    },
    ProviderDef {
        name: "llamacpp",
        env_prefix: "LLAMACPP",
        default_base_url: "http://localhost:8080/v1",
    },
    ProviderDef {
        name: "kimi_oauth",
        env_prefix: "KIMI_OAUTH",
        default_base_url: "https://api.kimi.com/coding/v1",
    },
    ProviderDef {
        name: "sumopod",
        env_prefix: "SUMOPOD",
        default_base_url: "https://ai.sumopod.com/v1",
    },
    ProviderDef {
        name: "cloudflare",
        env_prefix: "CLOUDFLARE",
        default_base_url: "https://api.cloudflare.com/client/v4/accounts",
    },
    ProviderDef {
        name: "custom",
        env_prefix: "CUSTOM",
        default_base_url: "",
    },
];

/// Runtime settings consumed by all proxy modules.
///
/// Built from `PanelConfig` (the active profile) + env-only vars
/// (`HOST`, `PORT`, `ANTHROPIC_AUTH_TOKEN`, `RUST_LOG`).
#[derive(Clone, Debug)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub anthropic_auth_token: Option<String>,
    // Model mapping (first model in each tier — for backward compat display)
    pub model: String,
    pub model_opus: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_haiku: Option<String>,
    // Rate limiting
    pub provider_rate_limit: u32,
    pub provider_rate_window: u64,
    pub provider_max_concurrency: usize,
    // Timeouts
    pub http_read_timeout: u64,
    pub http_connect_timeout: u64,
    // Feature flags
    pub enable_ip_rotation: bool,
    pub enable_network_probe_mock: bool,
    pub enable_title_generation_skip: bool,
    pub enable_suggestion_mode_skip: bool,
    pub fast_prefix_detection: bool,
    pub enable_filepath_extraction_mock: bool,
    pub enable_tool_retry: bool,
    pub tool_retry_max: u32,
    pub enable_rtk: bool,
    pub override_system_prompt: Option<String>,
    // Special providers (auto-enabled from keys)
    pub puter_api_key: Option<String>,
    pub kimi_oauth_enable: bool,
    pub cloudflare_api_key: Option<String>,
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_or_none(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

impl Settings {
    /// Build settings from a `PanelConfig` + environment-only vars.
    pub fn from_panel_config(config: &PanelConfig) -> Self {
        let profile = config.active_profile();
        let mm = &profile.model_mapping;

        // First model in each tier for display/backward compat
        let first_default = crate::panel_config::parse_model_list(&mm.default)
            .into_iter()
            .next()
            .unwrap_or_default();
        let first_opus = crate::panel_config::parse_model_list(&mm.opus)
            .into_iter()
            .next();
        let first_sonnet = crate::panel_config::parse_model_list(&mm.sonnet)
            .into_iter()
            .next();
        let first_haiku = crate::panel_config::parse_model_list(&mm.haiku)
            .into_iter()
            .next();

        // Special providers: check if keys exist in config
        let puter_key = profile.provider_keys.get("puter").cloned();
        let kimi_oauth_enabled = profile.provider_keys.contains_key("kimi_oauth")
            || env::var("KIMI_OAUTH_ENABLE")
                .ok()
                .is_some_and(|v| v == "true");
        let cloudflare_key = profile.provider_keys.get("cloudflare").and_then(|k| {
            let keys = k.split(';').next().map(|s| s.trim().to_string());
            keys.filter(|s| !s.is_empty())
        });

        Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_or("PORT", "8082").parse().unwrap_or(8082),
            anthropic_auth_token: env_or_none("ANTHROPIC_AUTH_TOKEN"),
            model: first_default,
            model_opus: first_opus,
            model_sonnet: first_sonnet,
            model_haiku: first_haiku,
            provider_rate_limit: profile.rate_limiting.provider_rate_limit,
            provider_rate_window: profile.rate_limiting.provider_rate_window,
            provider_max_concurrency: profile.rate_limiting.provider_max_concurrency,
            http_read_timeout: profile.timeouts.http_read_timeout,
            http_connect_timeout: profile.timeouts.http_connect_timeout,
            enable_ip_rotation: profile.features.enable_ip_rotation,
            enable_network_probe_mock: profile.features.enable_network_probe_mock,
            enable_title_generation_skip: profile.features.enable_title_generation_skip,
            enable_suggestion_mode_skip: profile.features.enable_suggestion_mode_skip,
            fast_prefix_detection: profile.features.fast_prefix_detection,
            enable_filepath_extraction_mock: profile.features.enable_filepath_extraction_mock,
            enable_tool_retry: profile.features.enable_tool_retry,
            tool_retry_max: profile.features.tool_retry_max,
            enable_rtk: profile.features.enable_rtk,
            override_system_prompt: profile.features.override_system_prompt.clone(),
            puter_api_key: puter_key,
            kimi_oauth_enable: kimi_oauth_enabled,
            cloudflare_api_key: cloudflare_key,
        }
    }

    /// Legacy constructor — kept for backward compat during migration.
    /// Delegates to `from_panel_config` after loading config.
    #[allow(dead_code)]
    pub fn from_env() -> Self {
        let config_path = crate::config_loader::config_path();
        let result = crate::config_loader::load_or_create(&config_path);
        Self::from_panel_config(&result.into_config())
    }

    pub fn resolve_model(&self, claude_model: &str) -> String {
        let lower = claude_model.to_lowercase();
        if lower.contains("opus")
            && let Some(ref m) = self.model_opus
        {
            return m.clone();
        }
        if lower.contains("haiku")
            && let Some(ref m) = self.model_haiku
        {
            return m.clone();
        }
        if lower.contains("sonnet")
            && let Some(ref m) = self.model_sonnet
        {
            return m.clone();
        }
        self.model.clone()
    }

    pub fn parse_provider_type(model_string: &str) -> &str {
        model_string.split('/').next().unwrap_or("openai")
    }

    pub fn parse_model_name(model_string: &str) -> &str {
        model_string
            .split_once('/')
            .map(|x| x.1)
            .unwrap_or(model_string)
    }
}

/// Get the base URL for a provider, checking config overrides then defaults.
#[allow(dead_code)]
pub fn get_provider_base_url_with_config(provider_name: &str, profile: &ProfileConfig) -> String {
    // Check profile override first
    if let Some(url) = profile.provider_base_urls.get(provider_name)
        && !url.is_empty()
    {
        return url.clone();
    }
    // Fallback to static defaults
    get_provider_default_base_url(provider_name)
}

/// Get the default base URL for a provider from the static list.
#[allow(dead_code)]
pub fn get_provider_default_base_url(provider_name: &str) -> String {
    PROVIDERS
        .iter()
        .find(|p| p.name == provider_name)
        .map(|d| d.default_base_url.to_string())
        .unwrap_or_default()
}

/// Legacy: get base URL from env vars (used by providers that haven't been migrated yet).
pub fn get_provider_base_url(provider_name: &str) -> String {
    let def = PROVIDERS.iter().find(|p| p.name == provider_name);
    let prefix = def.map(|d| d.env_prefix).unwrap_or("CUSTOM");
    let default_url = def.map(|d| d.default_base_url).unwrap_or("");

    let key = format!("{}_BASE_URL", prefix);
    env_or(&key, default_url)
}

/// Legacy: get API key from env vars (used by providers that haven't been migrated yet).
pub fn get_provider_api_key(provider_name: &str) -> Option<String> {
    let def = PROVIDERS.iter().find(|p| p.name == provider_name);
    let prefix = def.map(|d| d.env_prefix).unwrap_or("CUSTOM");

    let key = format!("{}_API_KEY", prefix);
    env_or_none(&key)
}
