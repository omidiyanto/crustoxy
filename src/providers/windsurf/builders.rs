//! Protobuf message builders for the Windsurf language server.
//!
//! Service: exa.language_server_pb.LanguageServerService
//!
//! Faithfully ported from WindsurfAPI/src/windsurf.js — field numbers and
//! nesting structure MUST match exactly or the LS binary will reject/ignore.

use uuid::Uuid;

use super::proto::*;

// ─── Enums ─────────────────────────────────────────────

pub const SOURCE_USER: u64 = 1;
pub const SOURCE_ASSISTANT: u64 = 3;

// ─── Timestamp ─────────────────────────────────────────

fn encode_timestamp() -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos() as u64;
    let mut parts = write_varint_field(1, secs);
    if nanos > 0 {
        parts.extend(write_varint_field(2, nanos));
    }
    parts
}

// ─── Metadata ──────────────────────────────────────────
// Matches WindsurfAPI/src/windsurf.js buildMetadata()

const DEFAULT_CLIENT_VERSION: &str = "2.0.67";

pub fn build_metadata(api_key: &str, session_id: &str) -> Vec<u8> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let hw = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let request_id: u64 = rand::random::<u64>() & 0xFFFF_FFFF_FFFF; // 48-bit

    let mut buf = Vec::new();
    buf.extend(write_string_field(1, "windsurf")); // ide_name
    buf.extend(write_string_field(2, DEFAULT_CLIENT_VERSION)); // extension_version
    buf.extend(write_string_field(3, api_key)); // api_key
    buf.extend(write_string_field(4, "en")); // locale
    buf.extend(write_string_field(5, os)); // os
    buf.extend(write_string_field(7, DEFAULT_CLIENT_VERSION)); // ide_version
    buf.extend(write_string_field(8, hw)); // hardware
    buf.extend(write_varint_field(9, request_id)); // request_id
    buf.extend(write_string_field(10, session_id)); // session_id
    buf.extend(write_string_field(12, "windsurf")); // extension_name
    buf
}

// ─── ChatMessage (for RawGetChatMessage) ───────────────

fn build_chat_message(content: &str, source: u64, conversation_id: &str) -> Vec<u8> {
    let msg_id = Uuid::new_v4().to_string();
    let mut parts = Vec::new();

    parts.extend(write_string_field(1, &msg_id)); // message_id
    parts.extend(write_varint_field(2, source)); // source enum
    parts.extend(write_message_field(3, &encode_timestamp())); // timestamp
    parts.extend(write_string_field(4, conversation_id)); // conversation_id

    if source == SOURCE_ASSISTANT {
        // Assistant: field 6 (action) → ChatMessageAction { ChatMessageActionGeneric { text } }
        let action_generic = write_string_field(1, content);
        let action = write_message_field(1, &action_generic);
        parts.extend(write_message_field(6, &action));
    } else {
        // User/System/Tool: field 5 (intent) → ChatMessageIntent { IntentGeneric { text } }
        let intent_generic = write_string_field(1, content);
        let intent = write_message_field(1, &intent_generic);
        parts.extend(write_message_field(5, &intent));
    }

    parts
}

// ─── RawGetChatMessageRequest ──────────────────────────

/// Build RawGetChatMessageRequest protobuf.
pub fn build_raw_get_chat_message_request(
    api_key: &str,
    messages: &[(String, String)], // (role, content) pairs
    system_prompt: &str,
    model_enum: u64,
    model_name: &str,
    session_id: &str,
) -> Vec<u8> {
    let conversation_id = Uuid::new_v4().to_string();
    let mut parts = Vec::new();

    // Field 1: Metadata
    parts.extend(write_message_field(1, &build_metadata(api_key, session_id)));

    // Field 2: repeated ChatMessage
    for (role, content) in messages {
        let source = match role.as_str() {
            "assistant" => SOURCE_ASSISTANT,
            "tool" => SOURCE_USER, // degrade tool to user
            _ => SOURCE_USER,
        };

        let text = if role == "assistant" {
            content.clone()
        } else if role == "tool" {
            format!("[tool result]: {}", content)
        } else {
            content.clone()
        };

        parts.extend(write_message_field(
            2,
            &build_chat_message(&text, source, &conversation_id),
        ));
    }

    // Field 3: system_prompt_override
    if !system_prompt.is_empty() {
        parts.extend(write_string_field(3, system_prompt));
    }

    // Field 4: model enum
    if model_enum > 0 {
        parts.extend(write_varint_field(4, model_enum));
    }

    // Field 5: chat_model_name
    if !model_name.is_empty() {
        parts.extend(write_string_field(5, model_name));
    }

    parts
}

// ─── Panel initialization ─────────────────────────────
// Matches WindsurfAPI/src/windsurf.js

/// Build InitializeCascadePanelStateRequest.
/// Field 1: metadata, Field 3: workspace_trusted (bool)
pub fn build_initialize_panel_state_request(api_key: &str, session_id: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend(write_message_field(1, &build_metadata(api_key, session_id)));
    buf.extend(write_bool_field(3, true)); // workspace_trusted
    buf
}

/// Build HeartbeatRequest. Field 1: metadata
pub fn build_heartbeat_request(api_key: &str, session_id: &str) -> Vec<u8> {
    write_message_field(1, &build_metadata(api_key, session_id))
}

/// Build AddTrackedWorkspaceRequest. Field 1: workspace path (string)
pub fn build_add_tracked_workspace_request(workspace_path: &str) -> Vec<u8> {
    write_string_field(1, workspace_path)
}

/// Build UpdateWorkspaceTrustRequest.
/// Field 1: metadata, Field 2: workspace_trusted (bool)
pub fn build_update_workspace_trust_request(api_key: &str, session_id: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend(write_message_field(1, &build_metadata(api_key, session_id)));
    buf.extend(write_bool_field(2, true)); // workspace_trusted
    buf
}

// ─── Cascade flow builders ─────────────────────────────

/// Build StartCascadeRequest.
/// Field 1: metadata, Field 4: source (1=CASCADE_CLIENT), Field 5: trajectory_type (1=USER_MAINLINE)
pub fn build_start_cascade_request(api_key: &str, session_id: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend(write_message_field(1, &build_metadata(api_key, session_id)));
    buf.extend(write_varint_field(4, 1)); // source = CASCADE_CLIENT
    buf.extend(write_varint_field(5, 1)); // trajectory_type = USER_MAINLINE
    buf
}

/// Build SendUserCascadeMessageRequest.
///
/// Matches WindsurfAPI/src/windsurf.js buildSendCascadeMessageRequest():
///   Field 1: cascade_id
///   Field 2: TextOrScopeItem { text = 1 }
///   Field 3: metadata
///   Field 5: cascade_config
pub fn build_send_cascade_message_request(
    api_key: &str,
    cascade_id: &str,
    text: &str,
    model_enum: u64,
    model_uid: &str,
    session_id: &str,
) -> Vec<u8> {
    let mut parts = Vec::new();

    // Field 1: cascade_id
    parts.extend(write_string_field(1, cascade_id));

    // Field 2: TextOrScopeItem { text = 1 }
    parts.extend(write_message_field(2, &write_string_field(1, text)));

    // Field 3: metadata
    parts.extend(write_message_field(3, &build_metadata(api_key, session_id)));

    // Field 5: cascade_config — the critical nested structure
    let cascade_config = build_cascade_config(model_enum, model_uid);
    parts.extend(write_message_field(5, &cascade_config));

    parts
}

/// Build CascadeConfig — the OUTER wrapper.
///
/// This is the most critical structure. It MUST match WindsurfAPI exactly:
///
/// ```text
/// CascadeConfig {
///   field 1: CascadePlannerConfig { ... }
///   field 5: MemoryConfig { enabled = false }
///   field 7: BrainConfig { ... }
/// }
/// ```
fn build_cascade_config(model_enum: u64, model_uid: &str) -> Vec<u8> {
    // ── Build CascadeConversationalPlannerConfig (inner) ──
    // planner_mode: NO_TOOL(3) — avoid Cascade's built-in tools
    let mut conv_parts = write_varint_field(4, 3); // planner_mode = NO_TOOL

    // field 10 (tool_calling_section): suppress built-in tool list
    let no_tool_section = {
        let mut s = write_varint_field(1, 1); // SECTION_OVERRIDE_MODE_OVERRIDE
        s.extend(write_string_field(2, "No tools are available."));
        s
    };
    conv_parts.extend(write_message_field(10, &no_tool_section));

    // field 12 (additional_instructions_section): direct-answer mode
    // This matches WindsurfAPI's comprehensive no-tool instructions
    let instructions_section = {
        let mut s = write_varint_field(1, 1); // OVERRIDE mode
        s.extend(write_string_field(
            2,
            concat!(
                "CRITICAL OPERATING CONSTRAINT — READ BEFORE ANY RESPONSE:\n",
                "You are being accessed as a plain chat API. You have NO tools, NO file access, ",
                "NO shell, NO code execution, NO repository awareness, NO ability to list or read ",
                "anything on the user's machine or any sandbox. You cannot \"check\", \"look at\", ",
                "\"open\", \"view\", \"inspect\", \"run\", \"glob\", \"grep\", \"list\", or \"edit\" anything.\n",
                "\n",
                "OUTPUT RULES:\n",
                "1. Never narrate tool-like actions (\"Let me check X\", \"I'll look at Y\").\n",
                "2. Never reference file paths, directory structures, line numbers, or repository ",
                "contents that were not explicitly pasted into the current conversation by the user.\n",
                "3. If the user asks about their code but hasn't pasted the relevant file content, ",
                "respond: \"I don't see that file in our conversation — please paste it and I'll help.\"\n",
                "4. For general questions, answer directly from your training knowledge.\n",
                "5. Match the user's language.\n",
                "\n",
                "Violating these rules will produce broken output for the end user. ",
                "Stay in chat-API mode at all times."
            ),
        ));
        s
    };
    conv_parts.extend(write_message_field(12, &instructions_section));

    // field 13 (communication_section): minimal override
    let comm_section = {
        let mut s = write_varint_field(1, 1); // OVERRIDE mode
        s.extend(write_string_field(
            2,
            "Respond clearly and concisely. Use markdown formatting when helpful.",
        ));
        s
    };
    conv_parts.extend(write_message_field(13, &comm_section));

    let conversational_config = conv_parts;

    // ── Build CascadePlannerConfig (wraps conversational config) ──
    let mut planner_parts = Vec::new();

    // field 2: CascadeConversationalPlannerConfig
    planner_parts.extend(write_message_field(2, &conversational_config));

    // Set BOTH modern uid (field 35) AND deprecated enum (field 15)
    // WindsurfAPI sets both for compatibility with different account states
    if !model_uid.is_empty() {
        planner_parts.extend(write_string_field(35, model_uid)); // requested_model_uid
        planner_parts.extend(write_string_field(34, model_uid)); // plan_model_uid (safety)
    }
    if model_enum > 0 {
        // requested_model_deprecated = ModelOrAlias { model = 1 (enum) }
        planner_parts.extend(write_message_field(15, &write_varint_field(1, model_enum)));
        // plan_model_deprecated = Model (enum directly at field 1)
        planner_parts.extend(write_varint_field(1, model_enum));
    }

    // field 6: max_output_tokens — CRITICAL! Without this, responses get truncated
    planner_parts.extend(write_varint_field(6, 32768));

    // field 11: code_changes_section — suppress IDE-specific boilerplate
    let empty_section = {
        let mut s = write_varint_field(1, 1); // OVERRIDE mode
        s.extend(write_string_field(2, ""));
        s
    };
    planner_parts.extend(write_message_field(11, &empty_section));

    let planner_config = planner_parts;

    // ── Build BrainConfig ──
    // Matches: writeMessageField(7, writeMessageField(6, writeMessageField(6, Buffer.alloc(0))))
    let brain_inner = write_message_field(6, &[]); // empty inner
    let brain_mid = write_message_field(6, &brain_inner);
    let brain_config = {
        let mut b = write_varint_field(1, 1);
        b.extend(brain_mid);
        b
    };

    // ── Build MemoryConfig ──
    // Matches: { enabled = false } — prevents LS injecting stored memories
    let memory_config = write_bool_field(1, false);

    // ── Assemble CascadeConfig ──
    let mut config = Vec::new();
    config.extend(write_message_field(1, &planner_config)); // field 1: planner config
    config.extend(write_message_field(5, &memory_config)); // field 5: memory config
    config.extend(write_message_field(7, &brain_config)); // field 7: brain config

    config
}

/// Build GetCascadeTrajectoryStepsRequest.
/// Field 1: cascade_id, Field 2: step_offset
pub fn build_get_trajectory_steps_request(cascade_id: &str, step_offset: u64) -> Vec<u8> {
    let mut parts = write_string_field(1, cascade_id);
    if step_offset > 0 {
        parts.extend(write_varint_field(2, step_offset));
    }
    parts
}

/// Build GetCascadeTrajectoryRequest.
/// Field 1: cascade_id
pub fn build_get_trajectory_request(cascade_id: &str) -> Vec<u8> {
    write_string_field(1, cascade_id)
}
