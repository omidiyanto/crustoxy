use serde_json::json;
use uuid::Uuid;

use crate::config::Settings;
use crate::models::anthropic::*;

pub fn try_optimizations(
    request: &MessagesRequest,
    settings: &Settings,
) -> Option<MessagesResponse> {
    if let Some(r) = try_quota_mock(request, settings) {
        return Some(r);
    }
    if let Some(r) = try_prefix_detection(request, settings) {
        return Some(r);
    }
    if let Some(r) = try_title_skip(request, settings) {
        return Some(r);
    }
    if let Some(r) = try_suggestion_skip(request, settings) {
        return Some(r);
    }
    if let Some(r) = try_filepath_mock(request, settings) {
        return Some(r);
    }
    None
}

fn try_quota_mock(request: &MessagesRequest, settings: &Settings) -> Option<MessagesResponse> {
    if !settings.enable_network_probe_mock {
        return None;
    }
    if request.max_tokens != Some(1) || request.messages.len() != 1 {
        return None;
    }
    let msg = &request.messages[0];
    if msg.role != "user" {
        return None;
    }
    let text = extract_text_from_content(&msg.content).to_lowercase();
    if !text.contains("quota") {
        return None;
    }
    tracing::info!("Optimization: Intercepted quota probe");
    Some(mock_response(&request.model, "Quota check passed.", 10, 5))
}

fn try_title_skip(request: &MessagesRequest, settings: &Settings) -> Option<MessagesResponse> {
    if !settings.enable_title_generation_skip {
        return None;
    }
    if request.system.is_none() || request.tools.is_some() {
        return None;
    }
    let system_text = extract_text_from_system(&request.system).to_lowercase();
    if system_text.contains("new conversation topic") && system_text.contains("title") {
        tracing::info!("Optimization: Skipped title generation");
        return Some(mock_response(&request.model, "Conversation", 100, 5));
    }
    None
}

fn try_prefix_detection(
    request: &MessagesRequest,
    settings: &Settings,
) -> Option<MessagesResponse> {
    if !settings.fast_prefix_detection {
        return None;
    }
    if request.messages.len() != 1 || request.messages[0].role != "user" {
        return None;
    }
    let content = extract_text_from_content(&request.messages[0].content);
    if !content.contains("<policy_spec>") || !content.contains("Command:") {
        return None;
    }
    let cmd = content
        .rfind("Command:")
        .map(|pos| content[pos + 8..].trim().to_string())
        .unwrap_or_default();

    let prefix = extract_command_prefix(&cmd);
    tracing::info!("Optimization: Fast prefix detection");
    Some(mock_response(&request.model, &prefix, 100, 5))
}

fn try_suggestion_skip(request: &MessagesRequest, settings: &Settings) -> Option<MessagesResponse> {
    if !settings.enable_suggestion_mode_skip {
        return None;
    }
    for msg in &request.messages {
        if msg.role == "user" {
            let text = extract_text_from_content(&msg.content);
            if text.contains("[SUGGESTION MODE:") {
                tracing::info!("Optimization: Skipped suggestion mode");
                return Some(mock_response(&request.model, "", 100, 1));
            }
        }
    }
    None
}

fn try_filepath_mock(request: &MessagesRequest, settings: &Settings) -> Option<MessagesResponse> {
    if !settings.enable_filepath_extraction_mock {
        return None;
    }
    if request.messages.len() != 1 || request.messages[0].role != "user" {
        return None;
    }
    if request.tools.is_some() {
        return None;
    }
    let content = extract_text_from_content(&request.messages[0].content);
    if !content.contains("Command:") || !content.contains("Output:") {
        return None;
    }
    let content_lower = content.to_lowercase();
    let system_text = extract_text_from_system(&request.system).to_lowercase();

    let user_has_filepaths =
        content_lower.contains("filepaths") || content_lower.contains("<filepaths>");
    let system_has_extract = system_text.contains("extract any file paths")
        || system_text.contains("file paths that this command");

    if !user_has_filepaths && !system_has_extract {
        return None;
    }

    let cmd_start = match content.find("Command:") {
        Some(pos) => pos + 8,
        None => return None,
    };
    let output_marker = match content[cmd_start..].find("Output:") {
        Some(pos) => cmd_start + pos,
        None => return None,
    };

    let command = content[cmd_start..output_marker].trim();
    let mut output = content[output_marker + 7..].trim().to_string();
    for marker in &["<", "\n\n"] {
        if let Some(pos) = output.find(marker) {
            output = output[..pos].trim().to_string();
        }
    }

    let filepaths = extract_filepaths(command, &output);
    tracing::info!("Optimization: Mocked filepath extraction");
    Some(mock_response(&request.model, &filepaths, 100, 10))
}

fn mock_response(
    model: &str,
    text: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> MessagesResponse {
    MessagesResponse {
        id: format!("msg_{}", Uuid::new_v4()),
        model: model.to_string(),
        role: "assistant".to_string(),
        content: vec![json!({"type": "text", "text": text})],
        msg_type: "message".to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Usage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    }
}

fn extract_command_prefix(cmd: &str) -> String {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    if let Some(pos) = first_word.rfind('/') {
        first_word[pos + 1..].to_string()
    } else {
        first_word.to_string()
    }
}

fn extract_filepaths(command: &str, output: &str) -> String {
    let mut paths = Vec::new();
    let combined = format!("{}\n{}", command, output);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || (trimmed.contains('/') && !trimmed.contains(' ') && !trimmed.starts_with('#'))
        {
            let path = trimmed
                .trim_start_matches(['+', '-', ' '])
                .trim();
            if !path.is_empty() && !paths.contains(&path.to_string()) {
                paths.push(path.to_string());
            }
        }
    }
    if paths.is_empty() {
        "<filepaths></filepaths>".to_string()
    } else {
        format!("<filepaths>\n{}\n</filepaths>", paths.join("\n"))
    }
}
