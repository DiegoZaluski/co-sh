pub use cosh::setup::DEFAULT_FALLBACKS;
pub use cosh::setup::{FallbackEntry, PromptCorrectorFallback};

pub fn default_fallbacks() -> Vec<FallbackEntry> {
    DEFAULT_FALLBACKS
        .iter()
        .map(|&(provider, model)| FallbackEntry {
            provider: provider.to_string(),
            model: model.to_string(),
        })
        .collect()
}

pub fn load_fallbacks(setup: &cosh::setup::Setup) -> Vec<FallbackEntry> {
    if setup.routing.fallbacks.is_empty() {
        default_fallbacks()
    } else {
        setup.routing.fallbacks.clone()
    }
}

pub fn save_fallbacks(setup: &mut cosh::setup::Setup, fallbacks: &[FallbackEntry]) {
    setup.routing.fallbacks = fallbacks.to_vec();
    setup.save();
}

pub fn load_prompt_corrector_fallbacks(setup: &cosh::setup::Setup) -> Vec<PromptCorrectorFallback> {
    setup.routing.fallback_prompt_corrector.clone()
}

pub fn save_prompt_corrector_fallbacks(
    setup: &mut cosh::setup::Setup,
    fallbacks: &[PromptCorrectorFallback],
) {
    setup.routing.fallback_prompt_corrector = fallbacks.to_vec();
    setup.save();
}
