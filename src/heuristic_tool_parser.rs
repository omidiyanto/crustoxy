use regex::Regex;
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
                    if let Some(idx) = self.buffer.find('●') {
                        filtered_parts.push(self.buffer[..idx].to_string());
                        self.buffer = self.buffer[idx..].to_string();
                        self.state = ParserState::MatchingFunction;
                    } else {
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

        detected
    }
}
