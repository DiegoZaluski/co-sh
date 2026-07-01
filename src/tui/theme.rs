use cosh_tui::core::lib::rgba::RGBA;

pub struct Theme {
    pub background: RGBA,
    pub background_panel: RGBA,
    pub background_element: RGBA,
    pub border: RGBA,
    pub border_active: RGBA,
    pub text: RGBA,
    pub text_muted: RGBA,
    pub primary: RGBA,
    pub secondary: RGBA,
    pub accent: RGBA,
    pub success: RGBA,
    pub warning: RGBA,
    pub error: RGBA,
    pub info: RGBA,
    pub diff_added: RGBA,
    pub diff_removed: RGBA,
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
            background: RGBA::from_hex("#0a0a0a"),
            background_panel: RGBA::from_hex("#141414"),
            background_element: RGBA::from_hex("#1e1e1e"),
            border: RGBA::from_hex("#484848"),
            border_active: RGBA::from_hex("#606060"),
            text: RGBA::from_hex("#eeeeee"),
            text_muted: RGBA::from_hex("#808080"),
            primary: RGBA::from_hex("#fab283"),
            secondary: RGBA::from_hex("#5c9cf5"),
            accent: RGBA::from_hex("#9d7cd8"),
            success: RGBA::from_hex("#7fd88f"),
            warning: RGBA::from_hex("#f5a742"),
            error: RGBA::from_hex("#e06c75"),
            info: RGBA::from_hex("#56b6c2"),
            diff_added: RGBA::from_hex("#4fd6be"),
            diff_removed: RGBA::from_hex("#c53b53"),
        }
    }
}
