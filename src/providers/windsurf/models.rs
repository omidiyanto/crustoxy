//! Windsurf model catalog — hardcoded enum values and model UIDs.
//!
//! Routing logic:
//!   modelUid present  → Cascade flow (StartCascade → SendUserCascadeMessage)
//!   only enumValue>0  → RawGetChatMessage (legacy)
//!
//! Ported from WindsurfAPI/src/models.js

#[derive(Debug, Clone)]
pub struct WindsurfModel {
    pub name: &'static str,
    pub enum_value: u64,
    pub model_uid: &'static str,
}

impl WindsurfModel {
    /// Cascade flow is used when model_uid is non-empty.
    pub fn use_cascade(&self) -> bool {
        !self.model_uid.is_empty()
    }
}

/// Hardcoded model catalog. Covers the most commonly used models.
/// Models with model_uid use Cascade flow; enum-only use RawGetChatMessage.
pub static MODELS: &[WindsurfModel] = &[
    // ── Claude ──
    WindsurfModel {
        name: "claude-4-sonnet",
        enum_value: 281,
        model_uid: "MODEL_CLAUDE_4_SONNET",
    },
    WindsurfModel {
        name: "claude-4-sonnet-thinking",
        enum_value: 282,
        model_uid: "MODEL_CLAUDE_4_SONNET_THINKING",
    },
    WindsurfModel {
        name: "claude-4-opus",
        enum_value: 290,
        model_uid: "MODEL_CLAUDE_4_OPUS",
    },
    WindsurfModel {
        name: "claude-4-opus-thinking",
        enum_value: 291,
        model_uid: "MODEL_CLAUDE_4_OPUS_THINKING",
    },
    WindsurfModel {
        name: "claude-4.1-opus",
        enum_value: 328,
        model_uid: "MODEL_CLAUDE_4_1_OPUS",
    },
    WindsurfModel {
        name: "claude-4.1-opus-thinking",
        enum_value: 329,
        model_uid: "MODEL_CLAUDE_4_1_OPUS_THINKING",
    },
    WindsurfModel {
        name: "claude-4.5-haiku",
        enum_value: 0,
        model_uid: "MODEL_PRIVATE_11",
    },
    WindsurfModel {
        name: "claude-4.5-sonnet",
        enum_value: 353,
        model_uid: "MODEL_PRIVATE_2",
    },
    WindsurfModel {
        name: "claude-4.5-sonnet-thinking",
        enum_value: 354,
        model_uid: "MODEL_PRIVATE_3",
    },
    WindsurfModel {
        name: "claude-4.5-opus",
        enum_value: 391,
        model_uid: "MODEL_CLAUDE_4_5_OPUS",
    },
    WindsurfModel {
        name: "claude-4.5-opus-thinking",
        enum_value: 392,
        model_uid: "MODEL_CLAUDE_4_5_OPUS_THINKING",
    },
    WindsurfModel {
        name: "claude-sonnet-4.6",
        enum_value: 0,
        model_uid: "claude-sonnet-4-6",
    },
    WindsurfModel {
        name: "claude-sonnet-4.6-thinking",
        enum_value: 0,
        model_uid: "claude-sonnet-4-6-thinking",
    },
    WindsurfModel {
        name: "claude-opus-4.6",
        enum_value: 0,
        model_uid: "claude-opus-4-6",
    },
    WindsurfModel {
        name: "claude-opus-4.6-thinking",
        enum_value: 0,
        model_uid: "claude-opus-4-6-thinking",
    },
    WindsurfModel {
        name: "claude-opus-4-7-medium",
        enum_value: 0,
        model_uid: "claude-opus-4-7-medium",
    },
    WindsurfModel {
        name: "claude-opus-4-7-high",
        enum_value: 0,
        model_uid: "claude-opus-4-7-high",
    },
    // ── GPT ──
    WindsurfModel {
        name: "gpt-4o",
        enum_value: 109,
        model_uid: "MODEL_CHAT_GPT_4O_2024_08_06",
    },
    WindsurfModel {
        name: "gpt-4.1",
        enum_value: 259,
        model_uid: "MODEL_CHAT_GPT_4_1_2025_04_14",
    },
    WindsurfModel {
        name: "gpt-5",
        enum_value: 340,
        model_uid: "MODEL_PRIVATE_6",
    },
    WindsurfModel {
        name: "gpt-5-medium",
        enum_value: 0,
        model_uid: "MODEL_PRIVATE_7",
    },
    WindsurfModel {
        name: "gpt-5-high",
        enum_value: 0,
        model_uid: "MODEL_PRIVATE_8",
    },
    WindsurfModel {
        name: "gpt-5-codex",
        enum_value: 346,
        model_uid: "MODEL_CHAT_GPT_5_CODEX",
    },
    WindsurfModel {
        name: "gpt-5.1",
        enum_value: 0,
        model_uid: "MODEL_PRIVATE_12",
    },
    WindsurfModel {
        name: "gpt-5.2",
        enum_value: 401,
        model_uid: "MODEL_GPT_5_2_MEDIUM",
    },
    // ── Gemini ──
    WindsurfModel {
        name: "gemini-2.5-pro",
        enum_value: 268,
        model_uid: "MODEL_GEMINI_2_5_PRO",
    },
    WindsurfModel {
        name: "gemini-2.5-flash",
        enum_value: 312,
        model_uid: "MODEL_GOOGLE_GEMINI_2_5_FLASH",
    },
    // ── Kimi ──
    WindsurfModel {
        name: "kimi-k2",
        enum_value: 323,
        model_uid: "MODEL_KIMI_K2",
    },
    WindsurfModel {
        name: "kimi-k2-thinking",
        enum_value: 394,
        model_uid: "MODEL_KIMI_K2_THINKING",
    },
    WindsurfModel {
        name: "kimi-k2.5",
        enum_value: 0,
        model_uid: "kimi-k2-5",
    },
    WindsurfModel {
        name: "kimi-k2-6",
        enum_value: 0,
        model_uid: "kimi-k2-6",
    },
    // ── DeepSeek ──
    WindsurfModel {
        name: "deepseek-v3",
        enum_value: 224,
        model_uid: "MODEL_DEEPSEEK_V3_0324",
    },
    WindsurfModel {
        name: "deepseek-r1",
        enum_value: 225,
        model_uid: "MODEL_DEEPSEEK_R1",
    },
    // ── Qwen ──
    WindsurfModel {
        name: "qwen-3",
        enum_value: 303,
        model_uid: "MODEL_QWEN3",
    },
    WindsurfModel {
        name: "qwen-3-coder",
        enum_value: 320,
        model_uid: "MODEL_QWEN3_CODER",
    },
    // ── GLM ──
    WindsurfModel {
        name: "glm-4.7",
        enum_value: 417,
        model_uid: "MODEL_GLM_4_7",
    },
    // ── Grok ──
    WindsurfModel {
        name: "grok-3",
        enum_value: 356,
        model_uid: "MODEL_GROK_3",
    },
    WindsurfModel {
        name: "grok-3-mini",
        enum_value: 357,
        model_uid: "MODEL_GROK_3_MINI",
    },
];

/// Look up a model by name (case-insensitive, supports common aliases).
pub fn resolve_model(name: &str) -> Option<&'static WindsurfModel> {
    let lower = name.to_lowercase();

    // Direct match
    if let Some(m) = MODELS.iter().find(|m| m.name == lower) {
        return Some(m);
    }

    // Common alias: dots → dashes (kimi-k2.5 → kimi-k2-5 won't match, but kimi-k2-5 handled)
    let dashed = lower.replace('.', "-");
    if let Some(m) = MODELS.iter().find(|m| m.name == dashed) {
        return Some(m);
    }

    // Try as model_uid directly
    MODELS
        .iter()
        .find(|m| m.model_uid.eq_ignore_ascii_case(&lower))
}
