#[allow(clippy::struct_excessive_bools)]
pub struct TuiConfig {
    pub scroll_acceleration: f64,
    pub show_scrollbar: bool,
    pub show_timestamps: bool,
    pub conceal: bool,
    pub show_tool_details: bool,
    pub show_generic_tool_output: bool,
    pub thinking_mode: bool,
    /// Monotonically increasing generation counter bumped on every theme change.
    /// Used as part of the message render cache key so cached cells with stale
    /// colors are invalidated when the user switches themes.
    pub theme_gen: u64,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            scroll_acceleration: 1.0,
            show_scrollbar: false,
            show_timestamps: false,
            conceal: false,
            show_tool_details: true,
            show_generic_tool_output: true,
            thinking_mode: true,
            theme_gen: 0,
        }
    }
}

pub struct LlmConfig {
    pub provider: String,
    pub model: Option<String>,
    /// Reasoning effort for the selected model: `None` (default) or one of
    /// `"low"`, `"medium"`, `"high"`. Mapped by the SDK onto each family's
    /// native knob (`reasoning_effort` / `thinkingConfig.thinkingLevel` /
    /// `thinking.budget_tokens`).
    pub reasoning: Option<String>,
}

impl LlmConfig {
    pub fn from_env() -> Self {
        // Try to get provider from env, otherwise auto-detect from available API keys
        let provider = std::env::var("COSH_PROVIDER")
            .ok()
            .or_else(|| cosh_sdk::connector::detect_provider().map(String::from))
            .unwrap_or_else(|| "openai".to_string());

        Self {
            provider,
            model: std::env::var("COSH_MODEL").ok(),
            reasoning: std::env::var("COSH_REASONING").ok(),
        }
    }
}

/// The cached models.dev catalog (maintained by the SDK's context discovery,
/// which the harness already fills on startup) is the authoritative offline
/// source for per-model reasoning metadata. The providers' own `/models`
/// endpoints do NOT expose it.
const MODELS_DEV_CACHE_DIR: Option<&str> = Some("cosh/cache");

/// Whether a model supports configurable reasoning/thinking.
///
/// Resolved by the SDK, fastest source first: the static table of known
/// models (exact ids — no disk read, no family catch-alls), then the
/// models.dev catalog cache (real `reasoning`/`reasoning_options` per model).
/// Models known to neither fall back to a name heuristic (OpenAI
/// `o1`/`o3`/`o4`/`gpt-5` series, DeepSeek R1, Kimi k2 thinking, Gemini 3.x,
/// Claude 4.x, GLM thinking models, ...).
pub fn model_supports_reasoning(model: &str) -> bool {
    if let Some(meta) = cosh_sdk::connector::model_reasoning(model, MODELS_DEV_CACHE_DIR) {
        return meta.supported;
    }
    heuristic_model_supports_reasoning(model)
}

/// Reasoning levels offered for a model, in display order. The first entry is
/// always `default` (model default). When the static table or the models.dev
/// catalog advertises specific effort values for the model, those are used;
/// otherwise the standard low/medium/high set is offered.
pub fn model_reasoning_levels(model: &str) -> Vec<String> {
    let mut levels = vec!["default".to_string()];
    if let Some(meta) = cosh_sdk::connector::model_reasoning(model, MODELS_DEV_CACHE_DIR)
        && let Some(efforts) = meta.efforts
    {
        for e in efforts {
            if e != "default" && !levels.iter().any(|l| l == &e) {
                levels.push(e);
            }
        }
    } else {
        levels.extend(["low".to_string(), "medium".to_string(), "high".to_string()]);
    }
    levels
}

/// Name-based fallback for models the catalog does not know (or that predate
/// its reasoning metadata).
fn heuristic_model_supports_reasoning(model: &str) -> bool {
    let m = model.to_lowercase();
    // OpenAI reasoning series.
    if m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
        || m.starts_with("gpt-oss")
        || m.contains("-reasoning")
    {
        return true;
    }
    // DeepSeek / Kimi / GLM / Qwen reasoning-tagged ids.
    if m.contains("deepseek-r1")
        || m.contains("kimi-k2-thinking")
        || m.contains("glm-5")
        || m.contains("glm-z1")
        || m.contains("qwen3")
    {
        return true;
    }
    // Gemini 3.x and Claude 4.x both ship a thinking knob.
    if m.starts_with("gemini-3") || m.starts_with("claude-sonnet-4") || m.starts_with("claude-opus-4")
    {
        return true;
    }
    false
}
