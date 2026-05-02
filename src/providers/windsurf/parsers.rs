//! Response parsers for Windsurf language server protobuf messages.
//!
//! Ported from WindsurfAPI/src/windsurf.js

use super::proto::*;

// ─── RawGetChatMessage response ────────────────────────

#[derive(Debug, Clone)]
pub struct RawChatResponse {
    pub text: String,
    pub in_progress: bool,
    pub is_error: bool,
}

/// Parse RawGetChatMessageResponse → extract text from RawChatMessage.
pub fn parse_raw_response(buf: &[u8]) -> RawChatResponse {
    let fields = parse_fields(buf);
    let f1 = match get_field_typed(&fields, 1, 2) {
        Some(f) => f,
        None => {
            return RawChatResponse {
                text: String::new(),
                in_progress: false,
                is_error: false,
            };
        }
    };

    let inner = parse_fields(&f1.bytes_value);
    let text = get_field_typed(&inner, 5, 2)
        .map(|f| f.as_string())
        .unwrap_or_default();
    let in_progress = get_field_typed(&inner, 6, 0)
        .map(|f| f.varint_value != 0)
        .unwrap_or(false);
    let is_error = get_field_typed(&inner, 7, 0)
        .map(|f| f.varint_value != 0)
        .unwrap_or(false);

    RawChatResponse {
        text,
        in_progress,
        is_error,
    }
}

// ─── Cascade response parsers ──────────────────────────

/// Parse StartCascadeResponse → cascade_id (field 1).
pub fn parse_start_cascade_response(buf: &[u8]) -> String {
    let fields = parse_fields(buf);
    get_field_typed(&fields, 1, 2)
        .map(|f| f.as_string())
        .unwrap_or_default()
}

/// Cascade trajectory status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryStatus {
    Unknown,
    Idle,       // 1 — trajectory complete
    Generating, // 2 — still producing output
    Other(u64),
}

impl From<u64> for TrajectoryStatus {
    fn from(v: u64) -> Self {
        match v {
            0 => TrajectoryStatus::Unknown,
            1 => TrajectoryStatus::Idle,
            2 => TrajectoryStatus::Generating,
            other => TrajectoryStatus::Other(other),
        }
    }
}

/// Parse GetCascadeTrajectoryResponse → status (field 2).
pub fn parse_trajectory_status(buf: &[u8]) -> TrajectoryStatus {
    let fields = parse_fields(buf);
    get_field_typed(&fields, 2, 0)
        .map(|f| TrajectoryStatus::from(f.varint_value))
        .unwrap_or(TrajectoryStatus::Unknown)
}

/// A parsed trajectory step.
#[derive(Debug, Clone)]
pub struct TrajectoryStep {
    pub step_type: u64,
    pub status: u64,
    pub text: String,
    pub thinking: String,
    pub error_text: String,
}

/// Step type constants.
pub const STEP_TYPE_PLANNER_RESPONSE: u64 = 15;

/// Step status constants.
pub const STEP_STATUS_DONE: u64 = 3;
pub const STEP_STATUS_GENERATING: u64 = 8;

/// Parse GetCascadeTrajectoryStepsResponse → extract planner response text.
pub fn parse_trajectory_steps(buf: &[u8]) -> Vec<TrajectoryStep> {
    let fields = parse_fields(buf);
    let steps: Vec<&ProtoField> = get_all_fields(&fields, 1)
        .into_iter()
        .filter(|f| f.wire_type == 2)
        .collect();

    let mut results = Vec::new();

    for step in steps {
        let sf = parse_fields(&step.bytes_value);
        let step_type = get_field_typed(&sf, 1, 0)
            .map(|f| f.varint_value)
            .unwrap_or(0);
        let status = get_field_typed(&sf, 4, 0)
            .map(|f| f.varint_value)
            .unwrap_or(0);

        let mut entry = TrajectoryStep {
            step_type,
            status,
            text: String::new(),
            thinking: String::new(),
            error_text: String::new(),
        };

        // CortexTrajectoryStep.planner_response = field 20
        if let Some(planner_field) = get_field_typed(&sf, 20, 2) {
            let pf = parse_fields(&planner_field.bytes_value);

            // response = field 1, modified_response = field 8
            let response_text = get_field_typed(&pf, 1, 2)
                .map(|f| f.as_string())
                .unwrap_or_default();
            let modified_text = get_field_typed(&pf, 8, 2)
                .map(|f| f.as_string())
                .unwrap_or_default();

            // Prefer modified_response when present
            entry.text = if !modified_text.is_empty() {
                modified_text
            } else {
                response_text
            };

            // thinking = field 3
            if let Some(think_field) = get_field_typed(&pf, 3, 2) {
                entry.thinking = think_field.as_string();
            }
        }

        // Error info: field 24 (error_message) or field 31 (error)
        if let Some(err_field) = get_field_typed(&sf, 24, 2) {
            let inner = parse_fields(&err_field.bytes_value);
            if let Some(details) = get_field_typed(&inner, 3, 2) {
                entry.error_text = read_error_details(&details.bytes_value);
            }
        }
        if entry.error_text.is_empty()
            && let Some(err_field) = get_field_typed(&sf, 31, 2)
        {
            entry.error_text = read_error_details(&err_field.bytes_value);
        }

        results.push(entry);
    }

    results
}

/// Extract error text from CortexErrorDetails.
fn read_error_details(buf: &[u8]) -> String {
    let ed = parse_fields(buf);
    // Try fields 1, 2, 3 (user_error_message, short_error, full_error)
    for fnum in [1, 2, 3] {
        if let Some(f) = get_field_typed(&ed, fnum, 2) {
            let s = f.as_string();
            let s = s.trim();
            if !s.is_empty() {
                // Take first line, max 300 chars
                return s.lines().next().unwrap_or(s).chars().take(300).collect();
            }
        }
    }
    String::new()
}
