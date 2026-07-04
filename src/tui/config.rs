#[allow(clippy::struct_excessive_bools)]
pub struct TuiConfig {
    pub scroll_acceleration: f64,
    pub show_scrollbar: bool,
    pub show_timestamps: bool,
    pub conceal: bool,
    pub show_tool_details: bool,
    pub show_generic_tool_output: bool,
    pub thinking_mode: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        TuiConfig {
            scroll_acceleration: 1.0,
            show_scrollbar: false,
            show_timestamps: false,
            conceal: false,
            show_tool_details: true,
            show_generic_tool_output: true,
            thinking_mode: false,
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
        let provider = std::env::var("COSH_PROVIDER").ok()
            .or_else(|| cosh_sdk::connector::detect_provider().map(String::from))
            .unwrap_or_else(|| "openai".to_string());

        LlmConfig {
            provider,
            model: std::env::var("COSH_MODEL").ok(),
        }
    }
}
