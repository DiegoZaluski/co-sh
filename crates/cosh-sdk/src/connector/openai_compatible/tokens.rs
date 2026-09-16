use crate::connector::usage::TokenUsage;

/// Extract completion tokens from a raw `OpenAI`-compatible API response.
///
/// Looks for `usage.completion_tokens` in the JSON response body — the
/// standard `OpenAI` shape that most compatible providers follow. Returns
/// `None` when the field is absent or zero (no completion yet).
#[must_use]
pub fn extract_tokens(raw: &str) -> Option<u32> {
    extract_usage(raw)
        .map(|u| u.output_tokens)
        .filter(|n| *n > 0)
}

/// Extract the full token usage from a raw `OpenAI`-compatible API response
/// or streaming usage frame, including cache and reasoning details when the
/// provider reports them (`prompt_tokens_details.cached_tokens`,
/// `completion_tokens_details.reasoning_tokens`). Returns `None` when no
/// usage object is present or the JSON is malformed.
#[must_use]
pub fn extract_usage(raw: &str) -> Option<TokenUsage> {
    #[derive(serde::Deserialize)]
    struct Details {
        cached_tokens: Option<u32>,
        reasoning_tokens: Option<u32>,
    }
    #[derive(serde::Deserialize)]
    struct Usage {
        #[serde(default)]
        prompt_tokens: u32,
        #[serde(default)]
        completion_tokens: u32,
        #[serde(default)]
        prompt_tokens_details: Option<Details>,
        #[serde(default)]
        completion_tokens_details: Option<Details>,
    }
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let usage: Usage = serde_json::from_value(v["usage"].clone()).ok()?;
    // OpenAI's `prompt_tokens` INCLUDES cached tokens
    // (prompt_tokens_details.cached_tokens ⊆ prompt_tokens), unlike
    // Anthropic where input excludes them. Normalize onto the shared
    // TokenUsage contract (input = NOT served from cache) so
    // total_input_tokens() stays correct.
    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    let reasoning = usage
        .completion_tokens_details
        .as_ref()
        .and_then(|d| d.reasoning_tokens)
        .unwrap_or(0);
    if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
        return None;
    }
    Some(TokenUsage {
        input_tokens: usage.prompt_tokens.saturating_sub(cache_read),
        output_tokens: usage.completion_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cache_read,
        reasoning_tokens: reasoning,
        reported_cost: None,
        reported_cost_credits: None,
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

    #[test]
    fn zero_completion_tokens_is_none() {
        let json = r#"{"usage":{"prompt_tokens":5,"completion_tokens":0}}"#;
        assert_eq!(extract_tokens(json), None);
    }

    #[test]
    fn extracts_cache_and_reasoning_details() {
        let json = r#"{"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":30}}}"#;
        let usage = extract_usage(json).unwrap();
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 80);
        assert_eq!(usage.reasoning_tokens, 30);
        assert_eq!(usage.total_input_tokens(), 100);
    }

    #[test]
    fn null_details_do_not_break_extraction() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":null}}}"#;
        let usage = extract_usage(json).unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn empty_usage_is_none() {
        assert_eq!(extract_usage(r#"{"usage":{}}"#), None);
    }

    #[test]
    fn absent_details_default_to_zero() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":2}}"#;
        let usage = extract_usage(json).unwrap();
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 0);
    }
}
