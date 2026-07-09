/// Extract completion tokens from a raw Claude API response.
///
/// Looks for `usage.output_tokens` in the JSON response body.
#[must_use]
pub fn extract_tokens(raw: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| {
            v["usage"]["output_tokens"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_output_tokens() {
        let json = r#"{"usage":{"input_tokens":10,"output_tokens":20}}"#;
        assert_eq!(extract_tokens(json), Some(20));
    }

    #[test]
    fn missing_usage_returns_none() {
        assert_eq!(extract_tokens(r#"{"id":"msg_1"}"#), None);
    }

    #[test]
    fn invalid_json_returns_none() {
        assert_eq!(extract_tokens("not json"), None);
    }
}
