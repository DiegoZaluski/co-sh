/// Extract completion tokens from a raw Gemini API response.
///
/// Looks for `usageMetadata.candidatesTokenCount` in the JSON response body
/// (camelCase keys from the Gemini API).
#[must_use]
pub fn extract_tokens(raw: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| {
            v["usageMetadata"]["candidatesTokenCount"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_candidates_token_count() {
        let json = r#"{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#;
        assert_eq!(extract_tokens(json), Some(20));
    }

    #[test]
    fn missing_usage_returns_none() {
        assert_eq!(extract_tokens(r#"{"candidates":[]}"#), None);
    }

    #[test]
    fn invalid_json_returns_none() {
        assert_eq!(extract_tokens("not json"), None);
    }
}
