use std::env;

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
        default_base_url: "https://api-inference.huggingface.co/v1",
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
        default_base_url: "https://api.siliconflow.cn/v1",
    },
    ProviderDef {
        name: "novita",
        env_prefix: "NOVITA",
        default_base_url: "https://api.novita.ai/v3/openai",
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
        name: "custom",
        env_prefix: "CUSTOM",
        default_base_url: "",
    },
];

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_or_none(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub model: String,
    pub model_opus: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_haiku: Option<String>,
    pub anthropic_auth_token: Option<String>,
    pub provider_rate_limit: u32,
    pub provider_rate_window: u64,
    pub provider_max_concurrency: usize,
    pub http_read_timeout: u64,
    pub http_connect_timeout: u64,
    pub enable_ip_rotation: bool,
    pub enable_network_probe_mock: bool,
    pub enable_title_generation_skip: bool,
    pub enable_suggestion_mode_skip: bool,
    pub fast_prefix_detection: bool,
    pub enable_filepath_extraction_mock: bool,
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            host: env_or("HOST", "0.0.0.0"),
            port: env_or("PORT", "8082").parse().unwrap_or(8082),
            model: env_or("MODEL", "openrouter/meta-llama/llama-3-8b-instruct:free"),
            model_opus: env_or_none("MODEL_OPUS"),
            model_sonnet: env_or_none("MODEL_SONNET"),
            model_haiku: env_or_none("MODEL_HAIKU"),
            anthropic_auth_token: env_or_none("ANTHROPIC_AUTH_TOKEN"),
            provider_rate_limit: env_or("PROVIDER_RATE_LIMIT", "40").parse().unwrap_or(40),
            provider_rate_window: env_or("PROVIDER_RATE_WINDOW", "60").parse().unwrap_or(60),
            provider_max_concurrency: env_or("PROVIDER_MAX_CONCURRENCY", "5").parse().unwrap_or(5),
            http_read_timeout: env_or("HTTP_READ_TIMEOUT", "300").parse().unwrap_or(300),
            http_connect_timeout: env_or("HTTP_CONNECT_TIMEOUT", "10").parse().unwrap_or(10),
            enable_ip_rotation: env_or("ENABLE_IP_ROTATION", "true").parse().unwrap_or(true),
            enable_network_probe_mock: env_or("ENABLE_NETWORK_PROBE_MOCK", "true")
                .parse()
                .unwrap_or(true),
            enable_title_generation_skip: env_or("ENABLE_TITLE_GENERATION_SKIP", "true")
                .parse()
                .unwrap_or(true),
            enable_suggestion_mode_skip: env_or("ENABLE_SUGGESTION_MODE_SKIP", "true")
                .parse()
                .unwrap_or(true),
            fast_prefix_detection: env_or("FAST_PREFIX_DETECTION", "true")
                .parse()
                .unwrap_or(true),
            enable_filepath_extraction_mock: env_or("ENABLE_FILEPATH_EXTRACTION_MOCK", "true")
                .parse()
                .unwrap_or(true),
        }
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

pub fn get_provider_base_url(provider_name: &str) -> String {
    let def = PROVIDERS.iter().find(|p| p.name == provider_name);
    let prefix = def.map(|d| d.env_prefix).unwrap_or("CUSTOM");
    let default_url = def.map(|d| d.default_base_url).unwrap_or("");

    let key = format!("{}_BASE_URL", prefix);
    env_or(&key, default_url)
}

pub fn get_provider_api_key(provider_name: &str) -> Option<String> {
    let def = PROVIDERS.iter().find(|p| p.name == provider_name);
    let prefix = def.map(|d| d.env_prefix).unwrap_or("CUSTOM");

    let key = format!("{}_API_KEY", prefix);
    env_or_none(&key)
}
