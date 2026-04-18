use crate::models::anthropic::*;
use crate::models::openai::*;

pub fn convert_messages(messages: &[Message]) -> Vec<ChatMessage> {
    let mut result = Vec::new();
    for msg in messages {
        match &msg.content {
            MessageContent::Text(s) => {
                result.push(ChatMessage {
                    role: msg.role.clone(),
                    content: Some(s.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }
            MessageContent::Blocks(blocks) => {
                if msg.role == "assistant" {
                    result.extend(convert_assistant_blocks(blocks));
                } else if msg.role == "user" {
                    result.extend(convert_user_blocks(blocks));
                }
            }
        }
    }
    result
}

fn convert_assistant_blocks(blocks: &[ContentBlock]) -> Vec<ChatMessage> {
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => content_parts.push(text.clone()),
            ContentBlock::Thinking { thinking } => {
                content_parts.push(format!("<think>\n{}\n</think>", thinking));
            }
            ContentBlock::ToolUse { id, name, input } => {
                let args = if input.is_object() || input.is_array() {
                    serde_json::to_string(input).unwrap_or_default()
                } else {
                    input.to_string()
                };
                tool_calls.push(ToolCallObj {
                    id: id.clone(),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: name.clone(),
                        arguments: args,
                    },
                });
            }
            _ => {}
        }
    }

    let content_str = content_parts.join("\n\n");
    let content = if content_str.is_empty() && tool_calls.is_empty() {
        Some(" ".to_string())
    } else if content_str.is_empty() {
        None
    } else {
        Some(content_str)
    };

    let tc = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    };

    vec![ChatMessage {
        role: "assistant".to_string(),
        content,
        tool_calls: tc,
        tool_call_id: None,
        reasoning_content: None,
    }]
}

fn convert_user_blocks(blocks: &[ContentBlock]) -> Vec<ChatMessage> {
    let mut result = Vec::new();
    let mut text_parts = Vec::new();

    let flush_text = |parts: &mut Vec<String>, out: &mut Vec<ChatMessage>| {
        if !parts.is_empty() {
            out.push(ChatMessage {
                role: "user".to_string(),
                content: Some(parts.join("\n")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
            parts.clear();
        }
    };

    for block in blocks {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => {
                flush_text(&mut text_parts, &mut result);
                let content_str = match content {
                    ToolResultContent::Text(s) => s.clone(),
                    ToolResultContent::Blocks(items) => items
                        .iter()
                        .filter_map(|item| {
                            item.get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                result.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(content_str),
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                    reasoning_content: None,
                });
            }
            _ => {}
        }
    }
    flush_text(&mut text_parts, &mut result);
    result
}

pub fn convert_system_prompt(system: &Option<SystemPrompt>) -> Option<ChatMessage> {
    match system {
        None => None,
        Some(SystemPrompt::Text(s)) if s.is_empty() => None,
        Some(SystemPrompt::Text(s)) => Some(ChatMessage {
            role: "system".to_string(),
            content: Some(s.clone()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }),
        Some(SystemPrompt::Blocks(blocks)) => {
            let text: String = blocks
                .iter()
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.is_empty() {
                None
            } else {
                Some(ChatMessage {
                    role: "system".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
        }
    }
}

pub fn convert_tools(tools: &[Tool]) -> Vec<ChatTool> {
    tools
        .iter()
        .map(|t| ChatTool {
            tool_type: "function".to_string(),
            function: ChatToolFunction {
                name: t.name.clone(),
                description: t.description.clone().unwrap_or_default(),
                parameters: t.input_schema.clone(),
            },
        })
        .collect()
}

pub fn build_openai_request(request: &MessagesRequest, model_name: &str) -> ChatCompletionRequest {
    let mut messages = Vec::new();
    if let Some(sys) = convert_system_prompt(&request.system) {
        messages.push(sys);
    }
    messages.extend(convert_messages(&request.messages));

    let tools = request.tools.as_ref().map(|t| convert_tools(t));

    ChatCompletionRequest {
        model: model_name.to_string(),
        messages,
        stream: true,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        stop: request.stop_sequences.clone(),
        tools,
        tool_choice: request.tool_choice.clone(),
    }
}

pub fn map_stop_reason(openai_reason: Option<&str>) -> &str {
    match openai_reason {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    }
}

pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32 / 4).max(1)
}

pub fn count_request_tokens(request: &MessagesRequest) -> u32 {
    let mut total: u32 = 0;

    if let Some(ref system) = request.system {
        let text = extract_text_from_system(&Some(system.clone()));
        total += estimate_tokens(&text) + 4;
    }

    for msg in &request.messages {
        let text = extract_text_from_content(&msg.content);
        total += estimate_tokens(&text);
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse {
                        name, input, id, ..
                    } => {
                        total += estimate_tokens(name);
                        total += estimate_tokens(&serde_json::to_string(input).unwrap_or_default());
                        total += estimate_tokens(id) + 15;
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let content_str = match content {
                            ToolResultContent::Text(s) => s.clone(),
                            ToolResultContent::Blocks(items) => {
                                serde_json::to_string(items).unwrap_or_default()
                            }
                        };
                        total += estimate_tokens(&content_str);
                        total += estimate_tokens(tool_use_id) + 8;
                    }
                    ContentBlock::Image { .. } => total += 765,
                    _ => {}
                }
            }
        }
        total += 4;
    }

    if let Some(ref tools) = request.tools {
        for tool in tools {
            let s = format!(
                "{}{}{}",
                tool.name,
                tool.description.as_deref().unwrap_or(""),
                serde_json::to_string(&tool.input_schema).unwrap_or_default()
            );
            total += estimate_tokens(&s) + 5;
        }
    }

    total.max(1)
}
