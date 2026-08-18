pub use crate::util::setup::FallbackEntry;

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

pub fn load_fallbacks(setup: &crate::util::setup::Setup) -> Vec<FallbackEntry> {
    if setup.routing.fallbacks.is_empty() {
        default_fallbacks()
    } else {
        setup.routing.fallbacks.clone()
    }
}

pub fn save_fallbacks(setup: &mut crate::util::setup::Setup, fallbacks: &[FallbackEntry]) {
    setup.routing.fallbacks = fallbacks.to_vec();
    setup.save();
}
