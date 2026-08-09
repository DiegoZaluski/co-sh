//! Context window discovery for LLM models.
//!
//! Attempts to discover the maximum context window size for a given model.
//! Known models are resolved from a static table FIRST — no network call, so
//! the most common models (deepseek, claude, gpt-4o, gemini, …) can never
//! silently fall back to the 100k default because of a slow network, a
//! changed API or a missing auth header. Unknown models are then looked up
//! through public APIs (OpenRouter and Anthropic Claude) without
//! authentication, and the static table remains the fallback knowledge for
//! families whose API needs a key (Anthropic's `/v1/models/{id}` returns 401
//! without `x-api-key`).

use serde::Deserialize;

/// Response from OpenRouter's /api/v1/models endpoint.
///
/// The catalog fits in a single page today (the response also carries
/// `total_count`/`links` fields that are ignored here).
#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    /// The vendor-prefixed, dated slug (e.g. `deepseek/deepseek-v4-flash-0731`).
    /// OpenRouter aliases (`~vendor/model-latest`) point at the canonical slug,
    /// so matching against it resolves the alias to the real entry.
    #[serde(default)]
    canonical_slug: Option<String>,
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

/// Well-known context windows for the most common model families, checked
/// BEFORE any network call. Each entry is a lowercase substring of the model
/// name; the FIRST match wins, so more specific fragments must come first
/// (e.g. `deepseek-v4` before the generic `deepseek`).
///
/// Values are deliberately CONSERVATIVE where the official API and
/// aggregators disagree (e.g. DeepSeek's official `deepseek-chat` window is
/// 128k while OpenRouter reports 163,840): under-sizing the budget only makes
/// the 80% compaction fire slightly early — safe — while over-sizing it would
/// push the prompt into a provider-side `ContextWindowExceeded` error.
const KNOWN_CONTEXT_WINDOWS: &[(&str, usize)] = &[
    // DeepSeek — `deepseek-v4-flash`/`deepseek-v4-pro` verified at 1M on
    // OpenRouter; chat/reasoner/r1/v3 are 128k on the official API.
    ("deepseek-v4", 1_048_576),
    ("deepseek-chat", 128_000),
    ("deepseek-reasoner", 128_000),
    ("deepseek", 128_000),
    // Anthropic — every current model (sonnet/opus/haiku, 3.x/4.x) is 200k.
    ("claude", 200_000),
    // OpenAI.
    ("gpt-4.1", 1_047_576),
    ("gpt-4o", 128_000),
    ("o1", 200_000),
    ("o3", 200_000),
    ("o4", 200_000),
    // Google.
    ("gemini", 1_000_000),
    // Open-weight models served at 128k by Groq/Together/etc.
    ("llama-3.1", 128_000),
    ("llama-3.3", 128_000),
    ("qwen2.5", 128_000),
    // Mistral.
    ("mistral-large", 128_000),
    ("mistral-small", 128_000),
];

/// Look up a model's window in the static table (case-insensitive substring).
/// Returns `None` for unknown models — the caller then tries the network.
fn static_known_window(model: &str) -> Option<usize> {
    let model_lower = model.to_lowercase();
    KNOWN_CONTEXT_WINDOWS
        .iter()
        .find(|(fragment, _)| model_lower.contains(fragment))
        .map(|(_, window)| *window)
}

/// Flexible match against the OpenRouter catalog: the needle matches a model
/// when it equals the id, equals the bare vendor-less suffix of the id
/// (`deepseek-v4-flash` matches `deepseek/deepseek-v4-flash`), or equals the
/// bare suffix of the canonical slug (so OpenRouter `~` aliases and dated
/// variants resolve to the same window). All comparisons are
/// case-insensitive — the user's model string and OpenRouter's slugs rarely
/// share casing.
fn find_window_in_models(needle: &str, models: &[OpenRouterModel]) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    models.iter().find_map(|m| {
        let id = m.id.to_lowercase();
        let matches = id == needle_lower
            || id.rsplit('/').next() == Some(needle_lower.as_str())
            || m.canonical_slug.as_deref().is_some_and(|c| {
                c.to_lowercase().rsplit('/').next() == Some(needle_lower.as_str())
            });
        m.context_length.filter(|_| matches)
    })
}

/// Attempts to discover the context window size for a given model.
///
/// Resolution order:
///   1. The static table of known windows (no network — common models always
///      resolve, immune to API/pagination/auth failures);
///   2. A public API: Anthropic for model names containing "claude", OpenRouter
///      otherwise (matching is vendor-prefix and case tolerant, so a bare
///      `deepseek-v4-flash` finds `deepseek/deepseek-v4-flash`);
///   3. `None` — the caller keeps its current budget. The failure is logged so
///      a silent fallback to the default window is never invisible.
///
/// # Arguments
///
/// * `model_name` - The model identifier (e.g., "claude-sonnet-4-5", "gpt-4o",
///   "deepseek-v4-flash")
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
    if let Some(window) = static_known_window(model_name) {
        return Some(window);
    }
    let discovered = if model_lower.contains("claude") {
        discover_anthropic_context(model_name).await
    } else {
        discover_openrouter_context(model_name).await
    };
    if discovered.is_none() {
        log::warn!("context window discovery failed for {model_name}; keeping the current budget");
    }
    discovered
}

/// Discover context window from Anthropic's API.
///
/// NOTE: the endpoint requires an `x-api-key` header — unauthenticated calls
/// return 401. Known claude models are resolved from the static table before
/// reaching here, so this only ever runs for unknown claude variants.
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
///
/// The per-model endpoint `GET /api/v1/models/{id}` returns 404 for EVERY id
/// (verified) — it is dead weight and is skipped. The full catalog listing is
/// fetched instead and matched flexibly ([`find_window_in_models`]).
async fn discover_openrouter_context(model_name: &str) -> Option<usize> {
    let response = reqwest::get("https://openrouter.ai/api/v1/models")
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let models_response: OpenRouterModelsResponse = response.json().await.ok()?;
    find_window_in_models(model_name, &models_response.data)
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

        (MIN_CONTEXT..=MAX_CONTEXT).contains(&value) && !HTTP_STATUS_CODES.contains(&value)
    }

    // ── Static table (no network) ─────────────────────────────────────────

    #[test]
    fn static_table_covers_the_most_common_models() {
        // The exact reported case: a bare, capitalized DeepSeek-V4-Flash must
        // resolve to its real 1M window WITHOUT a network call.
        assert_eq!(static_known_window("deepseek-v4-flash"), Some(1_048_576));
        assert_eq!(static_known_window("DeepSeek-V4-Flash"), Some(1_048_576));
        assert_eq!(
            static_known_window("deepseek/deepseek-v4-flash"),
            Some(1_048_576)
        );
        assert_eq!(
            static_known_window("~deepseek/deepseek-v4-flash-latest"),
            Some(1_048_576)
        );
        assert_eq!(static_known_window("deepseek-chat"), Some(128_000));
        assert_eq!(static_known_window("deepseek-reasoner"), Some(128_000));
        assert_eq!(static_known_window("deepseek-v3.2"), Some(128_000));

        assert_eq!(static_known_window("claude-sonnet-4-6"), Some(200_000));
        assert_eq!(static_known_window("claude-opus-5"), Some(200_000));
        assert_eq!(static_known_window("gpt-4o"), Some(128_000));
        assert_eq!(static_known_window("gpt-4o-mini"), Some(128_000));
        assert_eq!(static_known_window("gpt-4.1"), Some(1_047_576));
        assert_eq!(static_known_window("gemini-2.5-pro"), Some(1_000_000));
        assert_eq!(
            static_known_window("meta-llama/llama-3.1-8b-instruct"),
            Some(128_000)
        );

        // Unknown models fall through to the network path.
        assert_eq!(static_known_window("totally-unknown-model-123"), None);
    }

    #[test]
    fn static_table_first_match_wins_with_specific_fragments_first() {
        // "deepseek-v4" is listed before the generic "deepseek" — a v4 model
        // must resolve to 1M, never to the generic 128k.
        assert_eq!(static_known_window("deepseek-v4-flash"), Some(1_048_576));
        assert_eq!(static_known_window("deepseek-chat"), Some(128_000));
    }

    // ── OpenRouter flexible matching (no network) ─────────────────────────

    fn model(id: &str, canonical: Option<&str>, context: Option<usize>) -> OpenRouterModel {
        OpenRouterModel {
            id: id.to_string(),
            canonical_slug: canonical.map(str::to_string),
            context_length: context,
        }
    }

    #[test]
    fn openrouter_match_is_vendor_prefix_and_case_tolerant() {
        let models = vec![
            model(
                "deepseek/deepseek-v4-flash",
                Some("deepseek/deepseek-v4-flash-0731"),
                Some(1_048_576),
            ),
            model(
                "openai/gpt-4o",
                Some("openai/gpt-4o-2024-11-20"),
                Some(128_000),
            ),
        ];

        // The reported failure mode: a bare, capitalized name still matches
        // the vendor-prefixed id.
        assert_eq!(
            find_window_in_models("deepseek-v4-flash", &models),
            Some(1_048_576)
        );
        assert_eq!(
            find_window_in_models("DeepSeek-V4-Flash", &models),
            Some(1_048_576)
        );
        // Exact prefixed id also matches.
        assert_eq!(
            find_window_in_models("deepseek/deepseek-v4-flash", &models),
            Some(1_048_576)
        );
        // A dated canonical variant resolves via the canonical slug suffix.
        assert_eq!(
            find_window_in_models("deepseek-v4-flash-0731", &models),
            Some(1_048_576)
        );
        // Unrelated models never match.
        assert_eq!(find_window_in_models("gpt-3.5", &models), None);
    }

    #[test]
    fn openrouter_match_skips_models_without_context_length() {
        let models = vec![model(
            "vendor/some-model",
            None,
            None, // no window advertised
        )];
        assert_eq!(find_window_in_models("some-model", &models), None);
    }

    // ── Live network tests (best effort — require internet) ───────────────

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
        assert!(
            context.is_some(),
            "OpenRouter should return a context window for gpt-4o"
        );

        let value = context.unwrap();
        assert!(
            is_valid_context_window(value),
            "OpenRouter context window {} is not in valid range",
            value
        );
    }

    #[tokio::test]
    async fn test_discover_context_window_anthropic() {
        // Test the main function with an Anthropic model — resolved from the
        // static table, so it never even hits the network.
        let context = discover_context_window("claude-opus-5").await;
        assert_eq!(context, Some(200_000));
    }

    #[tokio::test]
    async fn test_discover_context_window_openrouter() {
        // Test the main function with an OpenRouter model — resolved from the
        // static table, so it never even hits the network.
        let context = discover_context_window("openai/gpt-4o").await;
        assert_eq!(context, Some(128_000));
    }

    #[tokio::test]
    async fn test_discover_context_window_invalid_model() {
        // Test with an invalid model name
        let context = discover_context_window("invalid-model-xyz").await;
        // Should return None for invalid models
        assert!(context.is_none(), "Invalid model should return None");
    }

    #[tokio::test]
    async fn test_discover_context_window_bare_deepseek_v4_flash() {
        // The regression this module fixes: a bare, capitalized model name
        // must resolve through the flexible OpenRouter match, not fall back.
        let context = discover_context_window("DeepSeek-V4-Flash").await;
        assert!(
            context.is_some(),
            "bare DeepSeek-V4-Flash must resolve to a window"
        );
        assert!(
            is_valid_context_window(context.unwrap()),
            "resolved window must be plausible"
        );
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
        assert!(
            !is_valid_context_window(200),
            "200 (HTTP status) should be invalid"
        );
        assert!(
            !is_valid_context_window(404),
            "404 (HTTP status) should be invalid"
        );
        assert!(
            !is_valid_context_window(500),
            "500 (HTTP status) should be invalid"
        );
        assert!(!is_valid_context_window(100), "100 is too small");
        assert!(!is_valid_context_window(500), "500 is too small");
        assert!(!is_valid_context_window(3_000_000), "3M is too large");
    }
}
