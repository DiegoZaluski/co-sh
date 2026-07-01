pub struct TuiConfig {
    pub scroll_acceleration: f64,
    pub show_scrollbar: bool,
    pub show_timestamps: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        TuiConfig {
            scroll_acceleration: 1.0,
            show_scrollbar: false,
            show_timestamps: false,
        }
    }
}
