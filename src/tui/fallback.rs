use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
}

pub const DEFAULT_FALLBACKS: &[(&str, &str)] = &[
    ("nvidia", "deepseek-ai/deepseek-v4-pro"),
    ("openrouter", "deepseek/deepseek-v4-pro"),
    ("groq", "openai/gpt-oss-120b"),
    ("charm", "deepseek-ai/deepseek-v4-pro"),
];

pub fn default_fallbacks() -> Vec<FallbackEntry> {
    DEFAULT_FALLBACKS
        .iter()
        .map(|&(provider, model)| FallbackEntry {
            provider: provider.to_string(),
            model: model.to_string(),
        })
        .collect()
}

const FALLBACK_KEY: &str = "fallback";

pub fn load_fallbacks(
    cache: &crate::util::cache::StaleCache<String, String>,
) -> Vec<FallbackEntry> {
    match cache.get(&FALLBACK_KEY.to_string()) {
        Some(raw) => serde_json::from_str(raw).unwrap_or_else(|_| default_fallbacks()),
        None => default_fallbacks(),
    }
}

pub fn save_fallbacks(
    cache: &mut crate::util::cache::StaleCache<String, String>,
    fallbacks: &[FallbackEntry],
) {
    let raw = serde_json::to_string(fallbacks).unwrap_or_default();
    cache.finish_revalidation(FALLBACK_KEY.to_string(), raw);
}
