use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::LazyLock;
use uuid::Uuid;

/// Sentinel control tokens leaked by some backends (e.g. `<|tool_call_end|>`).
static CONTROL_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<\|[^|>]{1,80}\|>").unwrap());

const CONTROL_TOKEN_START: &str = "<|";

/// A detected tool call from heuristic text parsing.
#[derive(Debug, Clone)]
pub struct DetectedTool {
    pub id: String,
    pub name: String,
    pub input: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ParserState {
    Text,
    MatchingFunction,
    ParsingParameters,
}

/// Stateful parser that detects raw text tool calls in the format:
/// `● <function=Name><parameter=key>value</parameter>...`
///
/// This is used as a fallback for models that emit tool calls as text
/// instead of using the structured API.
pub struct HeuristicToolParser {
    state: ParserState,
    buffer: String,
    current_tool_id: String,
    current_function_name: String,
    current_parameters: HashMap<String, String>,
}

static FUNC_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"●\s*<function=([^>]+)>").unwrap());

static PARAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<parameter=([^>]+)>(.*?)(?:</parameter>|$)").unwrap());

/// Pattern for `functions.Name:N{json_args}` format used by some models.
static FUNCTIONS_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"functions\.(\w+):\d+(\{[\s\S]*\})").unwrap());

impl HeuristicToolParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::Text,
            buffer: String::new(),
            current_tool_id: String::new(),
            current_function_name: String::new(),
            current_parameters: HashMap::new(),
        }
    }

    /// Strip complete sentinel control tokens from text.
    fn strip_control_tokens(text: &str) -> String {
        CONTROL_TOKEN_RE.replace_all(text, "").to_string()
    }

    /// Feed text into the parser.
    /// Returns `(filtered_text, detected_tool_calls)`.
    pub fn feed(&mut self, text: &str) -> (String, Vec<DetectedTool>) {
        self.buffer.push_str(text);
        self.buffer = Self::strip_control_tokens(&self.buffer);

        let mut detected_tools = Vec::new();
        let mut filtered_parts: Vec<String> = Vec::new();

        loop {
            match self.state {
                ParserState::Text => {
                    // Check for `functions.Name:N{json}` pattern first
                    if let Some(caps) = FUNCTIONS_CALL_RE.captures(&self.buffer) {
                        let full_match = caps.get(0).unwrap();
                        let func_name = caps[1].to_string();
                        let json_str = caps[2].to_string();

                        // Emit text before the match
                        let before = self.buffer[..full_match.start()].to_string();
                        if !before.is_empty() {
                            filtered_parts.push(before);
                        }

                        // Parse JSON arguments into input map
                        let mut input = HashMap::new();
                        if let Ok(parsed) = serde_json::from_str::<Value>(&json_str)
                            && let Some(obj) = parsed.as_object()
                        {
                            for (k, v) in obj {
                                let val = match v {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                input.insert(k.clone(), val);
                            }
                        }

                        detected_tools.push(DetectedTool {
                            id: format!("toolu_heuristic_{}", &Uuid::new_v4().to_string()[..8]),
                            name: func_name,
                            input,
                        });

                        self.buffer = self.buffer[full_match.end()..].to_string();
                        continue;
                    } else if let Some(idx) = self.buffer.find('●') {
                        filtered_parts.push(self.buffer[..idx].to_string());
                        self.buffer = self.buffer[idx..].to_string();
                        self.state = ParserState::MatchingFunction;
                    } else {
                        // Check for incomplete `functions.` at end of buffer
                        if let Some(start) = self.buffer.rfind("functions.") {
                            let tail = &self.buffer[start..];
                            // If it doesn't contain a closing `}`, it may be incomplete
                            if !tail.contains('}') {
                                let safe = self.buffer[..start].to_string();
                                self.buffer = self.buffer[start..].to_string();
                                if !safe.is_empty() {
                                    filtered_parts.push(safe);
                                }
                                break;
                            }
                        }
                        // Check for incomplete control token at end
                        if let Some(start) = self.buffer.rfind(CONTROL_TOKEN_START) {
                            let tail = &self.buffer[start..];
                            if !tail.contains("|>") {
                                // Incomplete token, hold it in buffer
                                let safe = self.buffer[..start].to_string();
                                self.buffer = self.buffer[start..].to_string();
                                if !safe.is_empty() {
                                    filtered_parts.push(safe);
                                }
                                break;
                            }
                        }
                        filtered_parts.push(std::mem::take(&mut self.buffer));
                        break;
                    }
                }
                ParserState::MatchingFunction => {
                    if let Some(m) = FUNC_START_RE.find(&self.buffer) {
                        let caps = FUNC_START_RE.captures(&self.buffer).unwrap();
                        self.current_function_name = caps[1].trim().to_string();
                        self.current_tool_id =
                            format!("toolu_heuristic_{}", &Uuid::new_v4().to_string()[..8]);
                        self.current_parameters.clear();
                        self.buffer = self.buffer[m.end()..].to_string();
                        self.state = ParserState::ParsingParameters;
                    } else if self.buffer.len() > 100 {
                        // Not a tool call, emit the bullet character and reset
                        filtered_parts.push(self.buffer[..1].to_string());
                        self.buffer = self.buffer[1..].to_string();
                        self.state = ParserState::Text;
                    } else {
                        break; // Need more data
                    }
                }
                ParserState::ParsingParameters => {
                    // Extract complete parameters
                    while let Some(caps) = PARAM_RE.captures(&self.buffer) {
                        let full_match = caps.get(0).unwrap();
                        if !caps[0].contains("</parameter>") {
                            break;
                        }
                        let key = caps[1].trim().to_string();
                        let val = caps[2].trim().to_string();
                        self.current_parameters.insert(key, val);
                        self.buffer = self.buffer[full_match.end()..].to_string();
                    }

                    let mut finished = false;

                    if self.buffer.contains('●') {
                        // Next tool call starting
                        let idx = self.buffer.find('●').unwrap();
                        if idx > 0 {
                            filtered_parts.push(self.buffer[..idx].to_string());
                            self.buffer = self.buffer[idx..].to_string();
                        }
                        finished = true;
                    } else if !self.buffer.is_empty()
                        && !self.buffer.trim_start().starts_with('<')
                        && !self.buffer.contains("<parameter=")
                    {
                        filtered_parts.push(std::mem::take(&mut self.buffer));
                        finished = true;
                    }

                    if finished {
                        detected_tools.push(DetectedTool {
                            id: self.current_tool_id.clone(),
                            name: self.current_function_name.clone(),
                            input: self.current_parameters.clone(),
                        });
                        self.state = ParserState::Text;
                    } else {
                        break;
                    }
                }
            }
        }

        (filtered_parts.join(""), detected_tools)
    }

    /// Flush any remaining tool calls in the buffer.
    pub fn flush(&mut self) -> Vec<DetectedTool> {
        self.buffer = Self::strip_control_tokens(&self.buffer);
        let mut detected = Vec::new();

        if self.state == ParserState::ParsingParameters {
            // Extract partial parameters
            for caps in PARAM_RE.captures_iter(&self.buffer) {
                let key = caps[1].trim().to_string();
                let val = caps[2].trim().to_string();
                self.current_parameters.insert(key, val);
            }

            detected.push(DetectedTool {
                id: self.current_tool_id.clone(),
                name: self.current_function_name.clone(),
                input: self.current_parameters.clone(),
            });
            self.state = ParserState::Text;
            self.buffer.clear();
        }

        // Check for any remaining `functions.Name:N{json}` in buffer
        if !self.buffer.is_empty()
            && let Some(caps) = FUNCTIONS_CALL_RE.captures(&self.buffer)
        {
            let func_name = caps[1].to_string();
            let json_str = caps[2].to_string();
            let mut input = HashMap::new();
            if let Ok(parsed) = serde_json::from_str::<Value>(&json_str)
                && let Some(obj) = parsed.as_object()
            {
                for (k, v) in obj {
                    let val = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    input.insert(k.clone(), val);
                }
            }
            detected.push(DetectedTool {
                id: format!("toolu_heuristic_{}", &Uuid::new_v4().to_string()[..8]),
                name: func_name,
                input,
            });
            self.buffer.clear();
        }

        detected
    }
}

// ═══ FALLBACK: GARBLED JSON RECOVERY ═══

static RE_TOOL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""name"\s*:\s*"([^"]+)""#).unwrap());

/// Pattern A: "parameter=key>value" (Often happens on Llama/Qwen)
static RE_PATTERN_A: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["\s,]?parameter=(\w+)>\s*(.*?)(?:</parameter>|$)"#).unwrap());

/// Pattern B: "<parameter_key>value" or "<parameter=key>value"
static RE_PATTERN_B: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<parameter[_=](\w+)>\s*(.*?)(?:</parameter|<|$)"#).unwrap());

/// Pattern C: JSON "arguments" malformed (missing closing bracket)
static RE_PATTERN_C_ARGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""arguments"\s*:\s*\{(.*)"#).unwrap());

/// Pattern C continued: Extract key-value pairs from arguments
static RE_PATTERN_C_KV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(\w+)"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap());

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RecoveredToolCall {
    pub name: String,
    pub arguments: Map<String, Value>,
}

/// Fallback: Attempts to reconstruct tool call from garbled JSON.
/// Only called when `serde_json::from_str` fails.
pub fn recover_garbled_tool_json(content: &str) -> Option<RecoveredToolCall> {
    // 1. Extract tool name (Required)
    let name = RE_TOOL_NAME
        .captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())?;

    let mut arguments = Map::new();

    // 2. Try Pattern A: "parameter=key>value"
    for caps in RE_PATTERN_A.captures_iter(content) {
        if let (Some(k), Some(v)) = (caps.get(1), caps.get(2)) {
            arguments.insert(
                k.as_str().to_string(),
                Value::String(
                    v.as_str()
                        .trim_end_matches('"')
                        .trim_end_matches('}')
                        .trim()
                        .to_string(),
                ),
            );
        }
    }

    // 3. Try Pattern B if A is empty: "<parameter_key>value"
    if arguments.is_empty() {
        for caps in RE_PATTERN_B.captures_iter(content) {
            if let (Some(k), Some(v)) = (caps.get(1), caps.get(2)) {
                arguments.insert(
                    k.as_str().to_string(),
                    Value::String(
                        v.as_str()
                            .trim_end_matches(']')
                            .trim_end_matches('"')
                            .trim()
                            .to_string(),
                    ),
                );
            }
        }
    }

    // 4. Try Pattern C if B is empty: Malformed JSON arguments
    if arguments.is_empty()
        && let Some(args_match) = RE_PATTERN_C_ARGS.captures(content)
    {
        let raw_args = args_match.get(1).unwrap().as_str();
        for kv in RE_PATTERN_C_KV.captures_iter(raw_args) {
            if let (Some(k), Some(v)) = (kv.get(1), kv.get(2)) {
                arguments.insert(
                    k.as_str().to_string(),
                    Value::String(v.as_str().to_string()),
                );
            }
        }
    }

    // 5. Try Pattern D if C is empty: Single-argument inference
    if arguments.is_empty() {
        let single_arg_tools = [
            ("Bash", "command"),
            ("Read", "file_path"),
            ("Write", "file_path"),
            ("Glob", "pattern"),
            ("Grep", "pattern"),
        ];

        if let Some(&(_, param_key)) = single_arg_tools.iter().find(|(t, _)| *t == name) {
            // Get all remaining text after the name declaration
            if let Some(name_match) = RE_TOOL_NAME.find(content) {
                let after_name = &content[name_match.end()..];

                // Clean up JSON noise characters ( { } " , : etc.) and parameter tags
                let cleaned = after_name
                    .trim_start_matches(|c: char| {
                        c.is_whitespace() || c == ',' || c == '"' || c == ':' || c == '{'
                    })
                    .trim_end_matches(|c: char| c.is_whitespace() || c == '"' || c == '}');

                let cleaned = RE_PATTERN_A.replace(cleaned, "$2").to_string();
                let cleaned = RE_PATTERN_B.replace(&cleaned, "$2").to_string();

                if cleaned.len() > 2 {
                    arguments.insert(
                        param_key.to_string(),
                        Value::String(cleaned.trim().to_string()),
                    );
                }
            }
        }
    }

    if arguments.is_empty() {
        return None;
    }

    Some(RecoveredToolCall { name, arguments })
}
