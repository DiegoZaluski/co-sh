pub mod logger;

use tiktoken_rs::CoreBPE;
use tiktoken_rs::tokenizer::{Tokenizer, get_tokenizer};

/// Which tiktoken encoding to use when estimating tokens.
///
/// The app talks to many providers with many different tokenizers; the best we
/// can do without per-provider vocabularies is route to the closest OpenAI
/// encoding by **model name** (the model string is what actually determines the
/// tokenizer, not the provider family — `gpt-4o` and `llama-3.3` are both
/// "OpenAI-compatible" providers) and fall back to `cl100k_base`, the classic
/// GPT-3.5/4 tokenizer and the de-facto standard for cross-model estimation.
///
/// The `*_singleton()` accessors cache the BPE tables in a `lazy_static`, so
/// the (relatively expensive) vocab load happens exactly once per encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenEncoding {
    /// GPT-3.5 / GPT-4 era; the default/fallback for every non-OpenAI model.
    Cl100k,
    /// GPT-4o / o1 / o3 / o4 / GPT-5 era.
    O200k,
    /// gpt-oss models (o200k_harmony).
    O200kHarmony,
    /// Code models (text-davinci / codex).
    P50k,
}

impl TokenEncoding {
    /// Pick the encoding that best matches the active model name.
    ///
    /// Known OpenAI models are resolved through tiktoken-rs's own registry
    /// ([`get_tokenizer`], kept in sync with OpenAI's tiktoken — covers
    /// `gpt-4.1`, the `o1`/`o3`/`o4` reasoning series, `gpt-oss`, `ft:`
    /// fine-tunes, exact aliases and prefixes). A small substring fallback
    /// covers bare aliases the registry does not list (e.g. `"gpt-4.5"`
    /// without a date suffix). Unknown models (claude, gemini, llama, …) and
    /// `None` fall back to [`TokenEncoding::Cl100k`], the de-facto standard
    /// for cross-provider estimation.
    pub fn for_model(model: Option<&str>) -> TokenEncoding {
        let Some(m) = model else {
            return TokenEncoding::Cl100k;
        };
        // Case-insensitive: some providers ship uppercase model names
        // (e.g. poe's default "GPT-4o").
        let m = m.to_ascii_lowercase();
        match get_tokenizer(&m) {
            Some(Tokenizer::O200kBase) => TokenEncoding::O200k,
            Some(Tokenizer::O200kHarmony) => TokenEncoding::O200kHarmony,
            Some(Tokenizer::P50kBase | Tokenizer::P50kEdit) => TokenEncoding::P50k,
            // r50k/gpt2 are legacy tokenizers (embeddings/code); the generic
            // cl100k estimate is the closest practical approximation.
            Some(Tokenizer::Cl100kBase | Tokenizer::R50kBase | Tokenizer::Gpt2) => {
                TokenEncoding::Cl100k
            }
            // Unknown to the registry — bare modern-OpenAI aliases (a rare
            // substring false positive on a non-OpenAI name is a harmless
            // estimation approximation; everything unknown still lands on
            // Cl100k).
            None if m.contains("gpt-4o")
                || m.contains("chatgpt-4o")
                || m.contains("gpt-4.5")
                || m.contains("gpt-5")
                || m.contains("o1")
                || m.contains("o3")
                || m.contains("o4") =>
            {
                TokenEncoding::O200k
            }
            None if m.contains("gpt-oss") => TokenEncoding::O200kHarmony,
            None if m.contains("davinci") || m.contains("codex") => TokenEncoding::P50k,
            None => TokenEncoding::Cl100k,
        }
    }

    /// Number of tokens `text` would occupy under this encoding.
    #[must_use]
    pub fn estimate(self, text: &str) -> usize {
        let bpe: &'static CoreBPE = match self {
            TokenEncoding::Cl100k => tiktoken_rs::cl100k_base_singleton(),
            TokenEncoding::O200k => tiktoken_rs::o200k_base_singleton(),
            TokenEncoding::O200kHarmony => tiktoken_rs::o200k_harmony_singleton(),
            TokenEncoding::P50k => tiktoken_rs::p50k_base_singleton(),
        };
        bpe.encode_ordinary(text).len()
    }
}

/// Estimate the number of tokens in a text using the default [`TokenEncoding::Cl100k`]
/// encoding (GPT-3.5/4 tokenizer) — the de-facto standard for a rough,
/// provider-agnostic estimate.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    TokenEncoding::Cl100k.estimate(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_resolves_openai_families() {
        // Newer OpenAI families → o200k.
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4o")),
            TokenEncoding::O200k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4o-mini")),
            TokenEncoding::O200k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4.1")),
            TokenEncoding::O200k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4.1-mini")),
            TokenEncoding::O200k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4.5")),
            TokenEncoding::O200k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("o3-mini")),
            TokenEncoding::O200k
        );
        assert_eq!(TokenEncoding::for_model(Some("o1")), TokenEncoding::O200k);
        // Uppercase model names (e.g. poe's default "GPT-4o").
        assert_eq!(
            TokenEncoding::for_model(Some("GPT-4o")),
            TokenEncoding::O200k
        );
        // gpt-oss uses the harmony tokenizer.
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-oss-20b")),
            TokenEncoding::O200kHarmony
        );
        // Legacy code models → p50k.
        assert_eq!(
            TokenEncoding::for_model(Some("text-davinci-003")),
            TokenEncoding::P50k
        );
    }

    #[test]
    fn routing_falls_back_to_cl100k() {
        // Legacy chat models keep cl100k.
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-4")),
            TokenEncoding::Cl100k
        );
        assert_eq!(
            TokenEncoding::for_model(Some("gpt-3.5-turbo")),
            TokenEncoding::Cl100k
        );
        // Non-OpenAI providers have no tiktoken vocabulary — generic estimate.
        for model in [
            "claude-sonnet-4-6",
            "gemini-2.0-flash",
            "llama-3.3-70b-versatile",
            "deepseek-chat",
            "grok-2-1212",
        ] {
            assert_eq!(TokenEncoding::for_model(Some(model)), TokenEncoding::Cl100k);
        }
        // No model configured → default.
        assert_eq!(TokenEncoding::for_model(None), TokenEncoding::Cl100k);
    }

    #[test]
    fn estimate_counts_are_sane_and_strictly_positive() {
        assert_eq!(estimate_tokens(""), 0);
        assert!(estimate_tokens("hello world") >= 1);
        assert!(estimate_tokens("The quick brown fox jumps over the lazy dog.") >= 8);
        assert!(estimate_tokens(&"word ".repeat(100)) >= 100);
    }
}
