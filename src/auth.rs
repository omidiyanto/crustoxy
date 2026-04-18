use axum::http::HeaderMap;

pub fn validate_api_key(
    headers: &HeaderMap,
    auth_token: &Option<String>,
) -> Result<(), &'static str> {
    let token = match auth_token {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(()),
    };

    let header_value = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .or_else(|| headers.get("anthropic-auth-token"))
        .and_then(|v| v.to_str().ok());

    let header_str = match header_value {
        Some(v) => v,
        None => return Err("Missing API key"),
    };

    let mut extracted = header_str;
    if let Some(stripped) = extracted.strip_prefix("Bearer ") {
        extracted = stripped;
    }
    if let Some(stripped) = extracted.strip_prefix("bearer ") {
        extracted = stripped;
    }

    if let Some(pos) = extracted.find(':') {
        extracted = &extracted[..pos];
    }

    if extracted == token.as_str() {
        Ok(())
    } else {
        Err("Invalid API key")
    }
}
