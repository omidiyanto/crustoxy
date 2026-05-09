use crate::converter::build_openai_request;
use crate::models::anthropic::MessagesRequest;
use serde_json::{Value, json};

pub fn build_kimi_request(req: &MessagesRequest, session_id: Option<&str>) -> Value {
    // 1. Base OpenAI-compat request
    let model_name = req.resolved_provider_model.as_deref().unwrap_or(&req.model);
    let model_str = crate::config::Settings::parse_model_name(model_name).to_string();
    let openai_req = build_openai_request(req, &model_str, "kimi_oauth");
    let mut body = serde_json::to_value(&openai_req).unwrap_or_else(|_| json!({}));

    if let Some(obj) = body.as_object_mut() {
        // Remove OpenAI specific stream options not needed by Kimi
        obj.remove("stream_options");

        // 2. Add Kimi-specific fields
        let mut effort = "medium";
        if let Some(ref config) = req.output_config
            && let Some(ref e) = config.effort
        {
            effort = match e.as_str() {
                "low" => "low",
                "high" => "high",
                "max" => "high", // Kimi doesn't have "max"
                _ => "medium",
            };
        }
        obj.insert("reasoning_effort".to_string(), json!(effort));

        // Always enable thinking for kimi-for-coding
        obj.insert("thinking".to_string(), json!({"type": "enabled"}));

        if let Some(sid) = session_id {
            obj.insert("prompt_cache_key".to_string(), json!(sid));
        }

        // Clamp max_tokens to 32000
        if let Some(m) = req.max_tokens
            && m > 32000
        {
            obj.insert("max_tokens".to_string(), json!(32000));
        }
    }

    body
}
