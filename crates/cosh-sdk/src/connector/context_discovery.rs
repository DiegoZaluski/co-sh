//! Context window discovery for LLM models.
//!
//! Attempts to discover the maximum context window size for a given model
//! by querying public APIs (OpenRouter and Anthropic Claude) without authentication.

use serde::Deserialize;

/// Response from OpenRouter's /api/v1/models endpoint.
#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    context_length: Option<usize>,
}

/// Response from Anthropic's /v1/models/{model_id} endpoint.
#[derive(Debug, Deserialize)]
struct AnthropicModelResponse {
    model_info: AnthropicModelInfo,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelInfo {
    max_input_tokens: Option<usize>,
}

/// Attempts to discover the context window size for a given model.
///
/// Uses a simple heuristic: if the model name contains "claude" (case-insensitive),
/// queries the Anthropic API; otherwise, queries the OpenRouter API.
///
/// # Arguments
///
/// * `model_name` - The model identifier (e.g., "claude-sonnet-4-5", "gpt-4o")
///
/// # Returns
///
/// * `Some(usize)` - The discovered context window size in tokens
/// * `None` - If the context window could not be discovered
///
/// # Notes
///
/// - Does not require authentication
/// - May fail due to network issues or API changes
/// - The caller should provide a sensible fallback value when this returns `None`
pub async fn discover_context_window(model_name: &str) -> Option<usize> {
    let model_lower = model_name.to_lowercase();

    if model_lower.contains("claude") {
        discover_anthropic_context(model_name).await
    } else {
        discover_openrouter_context(model_name).await
    }
}

/// Discover context window from Anthropic's API.
async fn discover_anthropic_context(model_name: &str) -> Option<usize> {
    let url = format!("https://api.anthropic.com/v1/models/{}", model_name);
    let response = reqwest::get(&url).await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    let model_response: AnthropicModelResponse = response.json().await.ok()?;
    model_response.model_info.max_input_tokens
}

/// Discover context window from OpenRouter's API.
async fn discover_openrouter_context(model_name: &str) -> Option<usize> {
    let url_openrouter = "https://openrouter.ai/api/v1/models";
    // Try specific model endpoint first
    let url = format!("{}/{}", url_openrouter, model_name);
    let response = reqwest::get(&url).await.ok()?;

    if response.status().is_success()
        && let Ok(model) = response.json::<OpenRouterModel>().await
        && let Some(context_length) = model.context_length
    {
        return Some(context_length);
    }

    // Fallback to listing all models and finding the match
    let response = reqwest::get(url_openrouter).await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    let models_response: OpenRouterModelsResponse = response.json().await.ok()?;
    models_response
        .data
        .iter()
        .find(|m| m.id == model_name)
        .and_then(|m| m.context_length)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to validate if a value looks like a valid context window size.
    /// Context windows are typically in the range of 1K to 2M tokens and should not
    /// be HTTP status codes (200, 400, 404, 500, etc).
    fn is_valid_context_window(value: usize) -> bool {
        // Reasonable context window range: 1K to 2M tokens
        const MIN_CONTEXT: usize = 1_000;
        const MAX_CONTEXT: usize = 2_000_000;

        // Common HTTP status codes that should not be mistaken for context windows
        const HTTP_STATUS_CODES: [usize; 10] = [200, 201, 204, 400, 401, 403, 404, 500, 502, 503];

        value >= MIN_CONTEXT
            && value <= MAX_CONTEXT
            && !HTTP_STATUS_CODES.contains(&value)
    }

    #[tokio::test]
    async fn test_discover_anthropic_context() {
        // Test with a known Anthropic model
        let context = discover_anthropic_context("claude-sonnet-4-5").await;
        // Anthropic API may require auth or be unavailable, so we just check
        // that the function doesn't crash and returns either Some or None
        if let Some(value) = context {
            assert!(
                is_valid_context_window(value),
                "Anthropic context window {} is not in valid range",
                value
            );
        }
    }

    #[tokio::test]
    async fn test_discover_openrouter_context() {
        // Test with a known OpenRouter model
        let context = discover_openrouter_context("openai/gpt-4o").await;
        // GPT-4o should have a context window
        assert!(context.is_some(), "OpenRouter should return a context window for gpt-4o");

        let value = context.unwrap();
        assert!(
            is_valid_context_window(value),
            "OpenRouter context window {} is not in valid range",
            value
        );
    }

    #[tokio::test]
    async fn test_discover_context_window_anthropic() {
        // Test the main function with an Anthropic model
        let context = discover_context_window("claude-opus-5").await;
        // Anthropic API may require auth, so we just check it doesn't crash
        if let Some(value) = context {
            assert!(
                is_valid_context_window(value),
                "Anthropic context window {} is not in valid range",
                value
            );
        }
    }

    #[tokio::test]
    async fn test_discover_context_window_openrouter() {
        // Test the main function with an OpenRouter model
        let context = discover_context_window("openai/gpt-4o").await;
        assert!(context.is_some(), "Should discover context window for gpt-4o");

        let value = context.unwrap();
        assert!(
            is_valid_context_window(value),
            "Context window {} is not in valid range",
            value
        );
    }

    #[tokio::test]
    async fn test_discover_context_window_invalid_model() {
        // Test with an invalid model name
        let context = discover_context_window("invalid-model-xyz").await;
        // Should return None for invalid models
        assert!(context.is_none(), "Invalid model should return None");
    }

    #[tokio::test]
    async fn test_openrouter_models_list() {
        // Test that we can list models from OpenRouter
        let response = reqwest::get("https://openrouter.ai/api/v1/models")
            .await
            .unwrap();
        assert!(response.status().is_success());

        let models: OpenRouterModelsResponse = response.json().await.unwrap();
        assert!(!models.data.is_empty());

        // Verify that at least some models have context_length
        let with_context = models
            .data
            .iter()
            .filter(|m| m.context_length.is_some())
            .count();
        assert!(with_context > 0, "Should have models with context_length");

        // Verify that context_length values are valid
        let valid_context_count = models
            .data
            .iter()
            .filter_map(|m| m.context_length)
            .filter(|&value| is_valid_context_window(value))
            .count();
        assert!(
            valid_context_count > 0,
            "Should have at least one model with valid context window"
        );
    }

    #[tokio::test]
    async fn test_context_window_validation_helper() {
        // Test the validation helper function
        assert!(is_valid_context_window(128000), "128K should be valid");
        assert!(is_valid_context_window(200000), "200K should be valid");
        assert!(is_valid_context_window(1000000), "1M should be valid");
        assert!(is_valid_context_window(1048576), "1M-ish should be valid");

        // Test invalid values
        assert!(!is_valid_context_window(200), "200 (HTTP status) should be invalid");
        assert!(!is_valid_context_window(404), "404 (HTTP status) should be invalid");
        assert!(!is_valid_context_window(500), "500 (HTTP status) should be invalid");
        assert!(!is_valid_context_window(100), "100 is too small");
        assert!(!is_valid_context_window(500), "500 is too small");
        assert!(!is_valid_context_window(3_000_000), "3M is too large");
    }
}
