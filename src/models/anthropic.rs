use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub tools: Option<Vec<Tool>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
    #[serde(default)]
    pub extra_body: Option<Value>,

    #[serde(skip)]
    pub original_model: Option<String>,
    #[serde(skip)]
    pub resolved_provider_model: Option<String>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: Value },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: ToolResultContent,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<Value>),
}

impl Default for ToolResultContent {
    fn default() -> Self {
        ToolResultContent::Text(String::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(default = "default_true_bool")]
    pub enabled: bool,
}

fn default_true_bool() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCountRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub tools: Option<Vec<Tool>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub role: String,
    pub content: Vec<Value>,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

pub fn extract_text_from_content(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => parts.push(text.clone()),
                    ContentBlock::Thinking { thinking } => parts.push(thinking.clone()),
                    _ => {}
                }
            }
            parts.join("\n")
        }
    }
}

pub fn extract_text_from_system(system: &Option<SystemPrompt>) -> String {
    match system {
        None => String::new(),
        Some(SystemPrompt::Text(s)) => s.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .filter_map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}
