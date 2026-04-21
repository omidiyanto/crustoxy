//! Tool Intent Detector — detects when a model expressed intent to use a tool
//! but failed to produce a structured tool call.
//!
//! This module is part of the **Auto-Retry Pipeline**. When the streaming
//! response ends without any tool calls but the accumulated text contains
//! tool-calling intent phrases, the proxy can re-prompt the provider.
//!
//! **Fail-safe**: If detection fails or produces false positives, the retry
//! simply produces the same text response — no harm done.

/// Intent phrases that suggest the model wanted to call a tool.
/// Case-insensitive matching is applied.
const INTENT_PHRASES: &[&str] = &[
    "here's the command",
    "here is the command",
    "let me run",
    "i'll execute",
    "running the following",
    "let me read",
    "let me check",
    "i'll use the",
    "<tool_call>",
    "<function=",
    "bash(",
    "read(",
    "edit(",
    "write(",
];

/// Check if accumulated response text contains tool-calling intent.
///
/// This is a lightweight heuristic check:
/// - False positives are harmless (retry produces same text)
/// - False negatives leave behavior unchanged (no retry attempted)
pub fn has_tool_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    INTENT_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// Corrective prompt appended as a user message during retry.
/// Instructs the model to emit a proper structured tool call.
pub const RETRY_PROMPT: &str = "Your previous response tried to call a tool but the format was wrong. \
     Please call the tool now using the proper JSON tool_calls format. \
     Do NOT explain what you will do — just call the tool directly.";

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detects_command_intent() {
        assert!(has_tool_intent("Here's the command to list files:"));
        assert!(has_tool_intent("Here is the command to run:"));
        assert!(has_tool_intent("Let me run that for you"));
        assert!(has_tool_intent("I'll execute the following:"));
    }

    #[test]
    fn test_detects_tool_use_intent() {
        assert!(has_tool_intent("I'll use the Bash tool to check"));
        assert!(has_tool_intent("Let me read the file first"));
        assert!(has_tool_intent("Let me check the configuration"));
    }

    #[test]
    fn test_detects_tag_intent() {
        assert!(has_tool_intent("Some text before <tool_call>"));
        assert!(has_tool_intent("<function=Bash>some content</function>"));
    }

    #[test]
    fn test_detects_function_call_intent() {
        assert!(has_tool_intent("bash(ls -la)"));
        assert!(has_tool_intent("read(/src/main.rs)"));
        assert!(has_tool_intent("edit(file.txt)"));
        assert!(has_tool_intent("write(output.txt)"));
    }

    #[test]
    fn test_case_insensitive() {
        assert!(has_tool_intent("HERE'S THE COMMAND:"));
        assert!(has_tool_intent("Let Me Run this"));
    }

    #[test]
    fn test_no_false_positives() {
        assert!(!has_tool_intent("The answer is 42"));
        assert!(!has_tool_intent("Here is the explanation of the code:"));
        assert!(!has_tool_intent("This function reads configuration"));
        assert!(!has_tool_intent("I think we should refactor this"));
        assert!(!has_tool_intent("The build was successful"));
    }

    #[test]
    fn test_empty_string() {
        assert!(!has_tool_intent(""));
    }

    #[test]
    fn test_retry_prompt_not_empty() {
        assert!(!RETRY_PROMPT.is_empty());
        assert!(RETRY_PROMPT.contains("tool"));
    }
}
