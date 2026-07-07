/// Extract completion tokens from a raw `OpenAI`-compatible API response.
///
/// Looks for `usage.completion_tokens` in the JSON response body — the
/// standard `OpenAI` shape that most compatible providers follow.
#[must_use]
pub(crate) fn extract_tokens(raw: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| {
            v["usage"]["completion_tokens"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_completion_tokens() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        assert_eq!(extract_tokens(json), Some(20));
    }

    #[test]
    fn missing_usage_returns_none() {
        assert_eq!(extract_tokens(r#"{"choices":[]}"#), None);
    }

    #[test]
    fn invalid_json_returns_none() {
        assert_eq!(extract_tokens("not json"), None);
    }
}
