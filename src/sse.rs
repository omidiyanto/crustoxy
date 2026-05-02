use serde_json::{Value, json};
use std::collections::HashMap;

pub struct ToolCallState {
    pub block_index: i32,
    pub tool_id: String,
    pub name: String,
    pub contents: Vec<String>,
    pub started: bool,
}

pub struct ContentBlockManager {
    pub next_index: u32,
    pub thinking_index: i32,
    pub text_index: i32,
    pub thinking_started: bool,
    pub text_started: bool,
    pub tool_states: HashMap<i32, ToolCallState>,
}

impl ContentBlockManager {
    pub fn new() -> Self {
        Self {
            next_index: 0,
            thinking_index: -1,
            text_index: -1,
            thinking_started: false,
            text_started: false,
            tool_states: HashMap::new(),
        }
    }

    pub fn allocate_index(&mut self) -> u32 {
        let idx = self.next_index;
        self.next_index += 1;
        idx
    }

    pub fn register_tool_name(&mut self, index: i32, name: &str) {
        if let Some(state) = self.tool_states.get_mut(&index) {
            let prev = &state.name;
            if prev.is_empty() || name.starts_with(prev.as_str()) {
                state.name = name.to_string();
            } else if !prev.starts_with(name) {
                state.name = format!("{}{}", prev, name);
            }
        } else {
            self.tool_states.insert(
                index,
                ToolCallState {
                    block_index: -1,
                    tool_id: String::new(),
                    name: name.to_string(),
                    contents: Vec::new(),
                    started: false,
                },
            );
        }
    }
}

pub struct SSEBuilder {
    pub message_id: String,
    pub model: String,
    pub input_tokens: u32,
    pub blocks: ContentBlockManager,
    accumulated_text: Vec<String>,
    accumulated_reasoning: Vec<String>,
}

impl SSEBuilder {
    pub fn new(message_id: String, model: String, input_tokens: u32) -> Self {
        Self {
            message_id,
            model,
            input_tokens,
            blocks: ContentBlockManager::new(),
            accumulated_text: Vec::new(),
            accumulated_reasoning: Vec::new(),
        }
    }

    fn format_event(event_type: &str, data: &Value) -> String {
        format!(
            "event: {}\ndata: {}\n\n",
            event_type,
            serde_json::to_string(data).unwrap()
        )
    }

    pub fn message_start(&self) -> String {
        Self::format_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 1
                    }
                }
            }),
        )
    }

    pub fn message_delta(&self, stop_reason: &str, output_tokens: u32) -> String {
        Self::format_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": output_tokens
                }
            }),
        )
    }

    pub fn message_stop(&self) -> String {
        Self::format_event("message_stop", &json!({"type": "message_stop"}))
    }

    pub fn content_block_start(&self, index: u32, block_type: &str, extra: Value) -> String {
        let mut block = json!({"type": block_type});
        match block_type {
            "thinking" => {
                block["thinking"] = json!("");
            }
            "text" => {
                block["text"] = json!("");
            }
            "tool_use" => {
                block["id"] = extra.get("id").cloned().unwrap_or(json!(""));
                block["name"] = extra.get("name").cloned().unwrap_or(json!(""));
                block["input"] = json!({});
            }
            _ => {}
        }
        Self::format_event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": block
            }),
        )
    }

    pub fn content_block_delta(&self, index: u32, delta_type: &str, content: &str) -> String {
        let mut delta = json!({"type": delta_type});
        match delta_type {
            "thinking_delta" => {
                delta["thinking"] = json!(content);
            }
            "text_delta" => {
                delta["text"] = json!(content);
            }
            "input_json_delta" => {
                delta["partial_json"] = json!(content);
            }
            _ => {}
        }
        Self::format_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": delta
            }),
        )
    }

    pub fn content_block_stop(&self, index: u32) -> String {
        Self::format_event(
            "content_block_stop",
            &json!({
                "type": "content_block_stop",
                "index": index
            }),
        )
    }

    pub fn ensure_thinking_block(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if self.blocks.text_started {
            events.push(self.content_block_stop(self.blocks.text_index as u32));
            self.blocks.text_started = false;
        }
        if !self.blocks.thinking_started {
            let idx = self.blocks.allocate_index();
            self.blocks.thinking_index = idx as i32;
            self.blocks.thinking_started = true;
            events.push(self.content_block_start(idx, "thinking", json!({})));
        }
        events
    }

    pub fn ensure_text_block(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if self.blocks.thinking_started {
            events.push(self.content_block_stop(self.blocks.thinking_index as u32));
            self.blocks.thinking_started = false;
        }
        if !self.blocks.text_started {
            let idx = self.blocks.allocate_index();
            self.blocks.text_index = idx as i32;
            self.blocks.text_started = true;
            events.push(self.content_block_start(idx, "text", json!({})));
        }
        events
    }

    pub fn emit_thinking_delta(&mut self, content: &str) -> String {
        self.accumulated_reasoning.push(content.to_string());
        self.content_block_delta(self.blocks.thinking_index as u32, "thinking_delta", content)
    }

    pub fn emit_text_delta(&mut self, content: &str) -> String {
        self.accumulated_text.push(content.to_string());
        self.content_block_delta(self.blocks.text_index as u32, "text_delta", content)
    }

    pub fn close_content_blocks(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if self.blocks.thinking_started {
            events.push(self.content_block_stop(self.blocks.thinking_index as u32));
            self.blocks.thinking_started = false;
        }
        if self.blocks.text_started {
            events.push(self.content_block_stop(self.blocks.text_index as u32));
            self.blocks.text_started = false;
        }
        events
    }

    pub fn close_all_blocks(&mut self) -> Vec<String> {
        let mut events = self.close_content_blocks();
        let tool_indices: Vec<i32> = self.blocks.tool_states.keys().cloned().collect();
        for tool_index in tool_indices {
            if let Some(state) = self.blocks.tool_states.get(&tool_index)
                && state.started
            {
                events.push(self.content_block_stop(state.block_index as u32));
            }
        }
        events
    }

    pub fn start_tool_block(&mut self, tool_index: i32, tool_id: &str, name: &str) -> String {
        let block_idx = self.blocks.allocate_index();
        if let Some(state) = self.blocks.tool_states.get_mut(&tool_index) {
            state.block_index = block_idx as i32;
            state.tool_id = tool_id.to_string();
            state.started = true;
        } else {
            self.blocks.tool_states.insert(
                tool_index,
                ToolCallState {
                    block_index: block_idx as i32,
                    tool_id: tool_id.to_string(),
                    name: name.to_string(),
                    contents: Vec::new(),
                    started: true,
                },
            );
        }
        self.content_block_start(block_idx, "tool_use", json!({"id": tool_id, "name": name}))
    }

    pub fn emit_tool_delta(&mut self, tool_index: i32, partial_json: &str) -> String {
        let block_idx = self
            .blocks
            .tool_states
            .get(&tool_index)
            .map(|s| s.block_index)
            .unwrap_or(0);
        if let Some(state) = self.blocks.tool_states.get_mut(&tool_index) {
            state.contents.push(partial_json.to_string());
        }
        self.content_block_delta(block_idx as u32, "input_json_delta", partial_json)
    }

    pub fn emit_error(&mut self, error_message: &str) -> Vec<String> {
        let idx = self.blocks.allocate_index();
        vec![
            self.content_block_start(idx, "text", json!({})),
            self.content_block_delta(idx, "text_delta", error_message),
            self.content_block_stop(idx),
        ]
    }

    pub fn estimate_output_tokens(&self) -> u32 {
        let text_tokens = self.accumulated_text.join("").len() as u32 / 4;
        let reasoning_tokens = self.accumulated_reasoning.join("").len() as u32 / 4;
        let tool_tokens: u32 = self
            .blocks
            .tool_states
            .values()
            .filter(|s| s.started)
            .map(|s| {
                let content_len: usize = s.contents.iter().map(|c| c.len()).sum();
                (s.name.len() as u32 / 4) + (content_len as u32 / 4) + 15
            })
            .sum();
        (text_tokens + reasoning_tokens + tool_tokens).max(1)
    }

    pub fn has_any_content(&self) -> bool {
        self.blocks.text_index >= 0 || !self.blocks.tool_states.is_empty()
    }
}

// ─── Standalone formatting helpers (for providers that manage their own stream) ──

fn format_sse(event_type: &str, data: &Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        event_type,
        serde_json::to_string(data).unwrap()
    )
}

pub fn format_message_start(request_id: &str, model: &str, input_tokens: u32) -> String {
    format_sse(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": request_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": 1
                }
            }
        }),
    )
}

pub fn format_content_delta(text: &str, index: u32) -> String {
    let mut events = String::new();

    // Emit content_block_start if this is the first delta (index 0 convention)
    if index == 0 {
        // We always emit as a text block start + delta pair per chunk.
        // The caller is expected to emit block_start once; subsequent calls
        // only emit deltas. For simplicity, we combine start+delta here.
    }

    events.push_str(&format_sse(
        "content_block_start",
        &json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    ));
    events.push_str(&format_sse(
        "content_block_delta",
        &json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
    ));
    events.push_str(&format_sse(
        "content_block_stop",
        &json!({
            "type": "content_block_stop",
            "index": 0
        }),
    ));

    events
}

pub fn format_block_start(index: u32, block_type: &str) -> String {
    let content_block = if block_type == "thinking" {
        json!({"type": "thinking", "thinking": ""})
    } else {
        json!({"type": "text", "text": ""})
    };
    format_sse(
        "content_block_start",
        &json!({
            "type": "content_block_start",
            "index": index,
            "content_block": content_block
        }),
    )
}

pub fn format_block_delta(index: u32, block_type: &str, delta_text: &str) -> String {
    let delta = if block_type == "thinking" {
        json!({"type": "thinking_delta", "thinking": delta_text})
    } else {
        json!({"type": "text_delta", "text": delta_text})
    };
    format_sse(
        "content_block_delta",
        &json!({
            "type": "content_block_delta",
            "index": index,
            "delta": delta
        }),
    )
}

pub fn format_block_stop(index: u32) -> String {
    format_sse(
        "content_block_stop",
        &json!({
            "type": "content_block_stop",
            "index": index
        }),
    )
}

pub fn format_message_stop(
    _request_id: &str,
    _model: &str,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: &str,
) -> String {
    let mut events = String::new();
    events.push_str(&format_sse(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
        }),
    ));
    events.push_str(&format_sse(
        "message_stop",
        &json!({"type": "message_stop"}),
    ));
    events
}

pub fn format_error_event(error_message: &str, error_type: &str) -> String {
    format_sse(
        "error",
        &json!({
            "type": "error",
            "error": {"type": error_type, "message": error_message}
        }),
    )
}
