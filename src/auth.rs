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

    let extracted = header_str
        .strip_prefix("Bearer ")
        .or_else(|| header_str.strip_prefix("bearer "))
        .unwrap_or(header_str)
        .trim();

    if constant_time_eq(extracted.as_bytes(), token.as_bytes()) {
        Ok(())
    } else {
        Err("Invalid API key")
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
