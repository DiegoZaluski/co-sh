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
            thinking_mode: false,
            theme_gen: 0,
        }
    }
}

pub struct LlmConfig {
    pub provider: String,
    pub model: Option<String>,
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
        }
    }
}
