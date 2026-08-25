//! Unified token-usage accounting across provider families.
//!
//! [`TokenUsage`] normalizes each provider's usage object onto common
//! fields, including the prompt-cache and reasoning counters that matter
//! for cost tracking:
//!
//! - **Claude**: `usage.input_tokens` / `output_tokens`,
//!   `cache_read_input_tokens` / `cache_creation_input_tokens`.
//! - **OpenAI-compatible**: `usage.prompt_tokens` / `completion_tokens`,
//!   `prompt_tokens_details.cached_tokens` (cache read) and
//!   `completion_tokens_details.reasoning_tokens`.
//! - **Gemini**: `usageMetadata.promptTokenCount` /
//!   `candidatesTokenCount`, `cachedContentTokenCount` (cache read) and
//!   `thoughtsTokenCount` (reasoning).
//!
//! Cache *creation* has no OpenAI/Gemini equivalent, so it stays zero for
//! those families.

/// Full token usage from an LLM response (or streaming usage frame),
/// normalized across provider families.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// Input tokens NOT served from or written to the prompt cache.
    #[serde(default)]
    pub input_tokens: u32,
    /// Completion tokens.
    #[serde(default)]
    pub output_tokens: u32,
    /// Tokens written to the prompt cache by this request (Anthropic only;
    /// billed at 1.25x base input price with the 5-minute TTL, 2x with 1h).
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// Tokens read from the prompt cache (billed at ~10% of base input
    /// price on Anthropic and OpenAI-compatible providers with caching
    /// enabled). This is where the caching savings show up.
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    /// Reasoning/thinking tokens, when the provider bills them separately
    /// from the visible completion (OpenAI `reasoning_tokens`, Gemini
    /// `thoughtsTokenCount`). Included in `output_tokens`.
    #[serde(default)]
    pub reasoning_tokens: u32,
}

impl TokenUsage {
    /// Total input tokens processed:
    /// `input + cache_creation + cache_read`.
    #[must_use]
    pub fn total_input_tokens(&self) -> u64 {
        u64::from(self.input_tokens)
            + u64::from(self.cache_creation_input_tokens)
            + u64::from(self.cache_read_input_tokens)
    }
}
