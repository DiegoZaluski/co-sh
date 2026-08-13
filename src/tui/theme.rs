use cosh_tui::core::lib::rgba::RGBA;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: RGBA,
    pub background_panel: RGBA,
    pub background_element: RGBA,
    pub background_menu: RGBA,
    pub border: RGBA,
    pub border_active: RGBA,
    pub border_subtle: RGBA,
    pub text: RGBA,
    pub text_muted: RGBA,
    pub selected_list_item_text: RGBA,
    pub primary: RGBA,
    pub secondary: RGBA,
    pub accent: RGBA,
    pub success: RGBA,
    pub warning: RGBA,
    pub error: RGBA,
    pub info: RGBA,
    pub diff_added: RGBA,
    pub diff_removed: RGBA,
    pub diff_context: RGBA,
    pub diff_hunk_header: RGBA,
    pub diff_highlight_added: RGBA,
    pub diff_highlight_removed: RGBA,
    pub diff_added_bg: RGBA,
    pub diff_removed_bg: RGBA,
    pub diff_context_bg: RGBA,
    pub diff_line_number: RGBA,
    pub diff_added_line_number_bg: RGBA,
    pub diff_removed_line_number_bg: RGBA,
    pub markdown_text: RGBA,
    pub markdown_heading: RGBA,
    pub markdown_link: RGBA,
    pub markdown_link_text: RGBA,
    pub markdown_code: RGBA,
    pub markdown_block_quote: RGBA,
    pub markdown_emph: RGBA,
    pub markdown_strong: RGBA,
    pub markdown_horizontal_rule: RGBA,
    pub markdown_list_item: RGBA,
    pub markdown_list_enumeration: RGBA,
    pub markdown_image: RGBA,
    pub markdown_image_text: RGBA,
    pub markdown_code_block: RGBA,
    pub syntax_comment: RGBA,
    pub syntax_keyword: RGBA,
    pub syntax_function: RGBA,
    pub syntax_variable: RGBA,
    pub syntax_string: RGBA,
    pub syntax_number: RGBA,
    pub syntax_type: RGBA,
    pub syntax_operator: RGBA,
    pub syntax_punctuation: RGBA,
    /// Background color of a "next agent loop" pending-queue row.
    pub queue_next_loop: RGBA,
    /// Background color of a "next request" pending-queue row.
    pub queue_next_request: RGBA,
    pub thinking_opacity: f64,
}

pub struct ThemeDef {
    pub name: &'static str,
    pub theme: Theme,
}

pub struct ThemeRegistry {
    pub themes: Vec<ThemeDef>,
    pub default_index: usize,
}

impl ThemeRegistry {
    pub fn new() -> Self {
        let themes = vec![
            ThemeDef {
                name: "cosh",
                theme: cosh(),
            },
            ThemeDef {
                name: "sakura",
                theme: sakura(),
            },
            ThemeDef {
                name: "neon",
                theme: neon(),
            },
            ThemeDef {
                name: "night-owl",
                theme: night_owl(),
            },
            ThemeDef {
                name: "jade",
                theme: jade(),
            },
            ThemeDef {
                name: "orng",
                theme: orng(),
            },
            ThemeDef {
                name: "opencode",
                theme: opencode(),
            },
            ThemeDef {
                name: "tokyonight",
                theme: tokyonight(),
            },
            ThemeDef {
                name: "catppuccin",
                theme: catppuccin(),
            },
            ThemeDef {
                name: "dracula",
                theme: dracula(),
            },
            ThemeDef {
                name: "nord",
                theme: nord(),
            },
            ThemeDef {
                name: "one-dark",
                theme: one_dark(),
            },
            ThemeDef {
                name: "gruvbox",
                theme: gruvbox(),
            },
            ThemeDef {
                name: "solarized",
                theme: solarized(),
            },
            ThemeDef {
                name: "monokai",
                theme: monokai(),
            },
            ThemeDef {
                name: "everforest",
                theme: everforest(),
            },
            ThemeDef {
                name: "kanagawa",
                theme: kanagawa(),
            },
            ThemeDef {
                name: "rosepine",
                theme: rosepine(),
            },
            ThemeDef {
                name: "github",
                theme: github(),
            },
            ThemeDef {
                name: "ayu",
                theme: ayu(),
            },
            ThemeDef {
                name: "material",
                theme: material(),
            },
        ];
        Self {
            default_index: 0,
            themes,
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.themes.iter().map(|t| t.name).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes
            .iter()
            .find(|t| t.name == name)
            .map(|t| &t.theme)
    }

    pub fn default_theme(&self) -> &Theme {
        &self.themes[self.default_index].theme
    }
}

// Theme definitions — ported from opencode TUI
//
// Helper to build full Theme
#[allow(clippy::too_many_arguments, clippy::similar_names)]
fn full_theme(
    bg: &str,
    bp: &str,
    be: &str,
    b: &str,
    ba: &str,
    bs: &str,
    tx: &str,
    tm: &str,
    pr: &str,
    se: &str,
    ac: &str,
    su: &str,
    wa: &str,
    er: &str,
    inf: &str,
    da: &str,
    dr: &str,
    dc: &str,
    dhh: &str,
    dha: &str,
    dhr: &str,
    dab: &str,
    drb: &str,
    dcb: &str,
    dln: &str,
    dalb: &str,
    drlb: &str,
    mt: &str,
    mh: &str,
    ml: &str,
    mlt: &str,
    mc: &str,
    mbq: &str,
    me: &str,
    ms: &str,
    mhr: &str,
    mli: &str,
    mle: &str,
    mi: &str,
    mit: &str,
    mcb: &str,
    sc: &str,
    sk: &str,
    sf: &str,
    sv: &str,
    sst: &str,
    sn: &str,
    st: &str,
    so: &str,
    sp: &str,
) -> Theme {
    Theme {
        background: RGBA::from_hex(bg),
        background_panel: RGBA::from_hex(bp),
        background_element: RGBA::from_hex(be),
        background_menu: RGBA::from_hex(be),
        border: RGBA::from_hex(b),
        border_active: RGBA::from_hex(ba),
        border_subtle: RGBA::from_hex(bs),
        text: RGBA::from_hex(tx),
        text_muted: RGBA::from_hex(tm),
        selected_list_item_text: RGBA::from_hex(bg),
        primary: RGBA::from_hex(pr),
        secondary: RGBA::from_hex(se),
        accent: RGBA::from_hex(ac),
        success: RGBA::from_hex(su),
        warning: RGBA::from_hex(wa),
        error: RGBA::from_hex(er),
        info: RGBA::from_hex(inf),
        diff_added: RGBA::from_hex(da),
        diff_removed: RGBA::from_hex(dr),
        diff_context: RGBA::from_hex(dc),
        diff_hunk_header: RGBA::from_hex(dhh),
        diff_highlight_added: RGBA::from_hex(dha),
        diff_highlight_removed: RGBA::from_hex(dhr),
        diff_added_bg: RGBA::from_hex(dab),
        diff_removed_bg: RGBA::from_hex(drb),
        diff_context_bg: RGBA::from_hex(dcb),
        diff_line_number: RGBA::from_hex(dln),
        diff_added_line_number_bg: RGBA::from_hex(dalb),
        diff_removed_line_number_bg: RGBA::from_hex(drlb),
        markdown_text: RGBA::from_hex(mt),
        markdown_heading: RGBA::from_hex(mh),
        markdown_link: RGBA::from_hex(ml),
        markdown_link_text: RGBA::from_hex(mlt),
        markdown_code: RGBA::from_hex(mc),
        markdown_block_quote: RGBA::from_hex(mbq),
        markdown_emph: RGBA::from_hex(me),
        markdown_strong: RGBA::from_hex(ms),
        markdown_horizontal_rule: RGBA::from_hex(mhr),
        markdown_list_item: RGBA::from_hex(mli),
        markdown_list_enumeration: RGBA::from_hex(mle),
        markdown_image: RGBA::from_hex(mi),
        markdown_image_text: RGBA::from_hex(mit),
        markdown_code_block: RGBA::from_hex(mcb),
        syntax_comment: RGBA::from_hex(sc),
        syntax_keyword: RGBA::from_hex(sk),
        syntax_function: RGBA::from_hex(sf),
        syntax_variable: RGBA::from_hex(sv),
        syntax_string: RGBA::from_hex(sst),
        syntax_number: RGBA::from_hex(sn),
        syntax_type: RGBA::from_hex(st),
        syntax_operator: RGBA::from_hex(so),
        syntax_punctuation: RGBA::from_hex(sp),
        queue_next_loop: RGBA::from_hex("#00000000"),
        queue_next_request: RGBA::from_hex("#00000000"),
        thinking_opacity: 0.6,
    }
}

fn cosh() -> Theme {
    let mut t = full_theme(
        "#07070A", "#0B0B12", "#11111C", "#777DA7", "#8B93C2", "#1E2030", "#E8F0FF", "#777DA7",
        "#777DE7", "#8B93C2", "#E8F0FF", "#5CB87A", "#D4A742", "#D4575A", "#5FD4CB", "#5CB87A",
        "#D4575A", "#4A4D5E", "#8B93C2", "#66BB6A", "#EF5350", "#0A1A0A", "#1A0A0A", "#07070A",
        "#4A4D5E", "#0A1A0A", "#1A0A0A", "#E8F0FF", "#777DA7", "#777DE7", "#E8F0FF", "#8B93C2",
        "#777DA7", "#D4A742", "#777DE7", "#1E2030", "#777DE7", "#8B93C2", "#777DE7", "#8B93C2",
        "#E8F0FF", "#4A4D5E", "#777DE7", "#8B93C2", "#E8F0FF", "#D4A742", "#CE93D8", "#BA68C8",
        "#8B93C2", "#E8F0FF",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3116");
    t.queue_next_request = RGBA::from_hex("#123A38");
    t
}

fn sakura() -> Theme {
    let mut t = full_theme(
        "#E8F0FF", "#DCE4F0", "#D0D8E4", "#C94C6E", "#D45D79", "#C8D0DC", "#343434", "#8B8B8B",
        "#D45D79", "#C94C6E", "#343434", "#7CB342", "#F4C2C2", "#E57373", "#D45D79", "#7CB342",
        "#E57373", "#9E9E9E", "#D45D79", "#66BB6A", "#EF9A9A", "#E8F5E9", "#FFF0F3", "#E8F0FF",
        "#9E9E9E", "#E8F5E9", "#FFF0F3", "#343434", "#D45D79", "#C94C6E", "#343434", "#D45D79",
        "#D45D79", "#F4C2C2", "#D45D79", "#C8D0DC", "#D45D79", "#C94C6E", "#D45D79", "#C94C6E",
        "#343434", "#9E9E9E", "#D45D79", "#D45D79", "#343434", "#F48FB1", "#CE93D8", "#BA68C8",
        "#D45D79", "#343434",
    );
    t.queue_next_loop = RGBA::from_hex("#F7E3C7");
    t.queue_next_request = RGBA::from_hex("#CDE6EF");
    t
}

fn neon() -> Theme {
    let mut t = full_theme(
        "#191716", "#1F1E1A", "#262520", "#1B9AAA", "#C04CFD", "#2E2B25", "#E8F0FF", "#8B95A7",
        "#C04CFD", "#1B9AAA", "#E8F0FF", "#1B9AAA", "#D97706", "#EF4444", "#C04CFD", "#1B9AAA",
        "#EF4444", "#8B95A7", "#C04CFD", "#34D399", "#F87171", "#0F1E18", "#1F1010", "#191716",
        "#8B95A7", "#0F1E18", "#1F1010", "#E8F0FF", "#C04CFD", "#1B9AAA", "#E8F0FF", "#C04CFD",
        "#1B9AAA", "#D97706", "#C04CFD", "#2E2B25", "#C04CFD", "#1B9AAA", "#C04CFD", "#1B9AAA",
        "#E8F0FF", "#8B95A7", "#C04CFD", "#1B9AAA", "#E8F0FF", "#D97706", "#F59E0B", "#2DD4BF",
        "#1B9AAA", "#E8F0FF",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2A14");
    t.queue_next_request = RGBA::from_hex("#233A3D");
    t
}

fn night_owl() -> Theme {
    let mut t = full_theme(
        "#011627", "#00111d", "#0b2942", "#5f7e97", "#82AAFF", "#122d42", "#d6deeb", "#5f7e97",
        "#82AAFF", "#c792ea", "#7fdbca", "#22da6e", "#ecc48d", "#EF5350", "#80CBC4", "#9CCC65",
        "#EF5350", "#5f7e97", "#a2bffc", "#c5e478", "#EF5350", "#1a2e1a", "#2e1a1a", "#00111d",
        "#4b6479", "#1a2e1a", "#2e1a1a", "#d6deeb", "#82AAFF", "#82AAFF", "#80CBC4", "#ecc48d",
        "#5f7e97", "#c792ea", "#82AAFF", "#5f7e97", "#c792ea", "#82AAFF", "#82AAFF", "#80CBC4",
        "#ecc48d", "#637777", "#c792ea", "#c792ea", "#c5e478", "#ecc48d", "#F78C6C", "#ffcb8b",
        "#7fdbca", "#d6deeb",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2E1C");
    t.queue_next_request = RGBA::from_hex("#152A3A");
    t
}

fn jade() -> Theme {
    let mut t = full_theme(
        "#0A0903", "#100E08", "#181610", "#04724D", "#059669", "#1C1A10", "#E8F0FF", "#6B7280",
        "#04724D", "#059669", "#E8F0FF", "#059669", "#D97706", "#EF4444", "#06B6D4", "#059669",
        "#EF4444", "#6B7280", "#04724D", "#10B981", "#F87171", "#0A1A10", "#1A1010", "#0A0903",
        "#6B7280", "#0A1A10", "#1A1010", "#E8F0FF", "#04724D", "#059669", "#E8F0FF", "#10B981",
        "#059669", "#D97706", "#059669", "#1C1A10", "#04724D", "#059669", "#04724D", "#E8F0FF",
        "#E8F0FF", "#6B7280", "#04724D", "#059669", "#E8F0FF", "#D97706", "#F59E0B", "#06B6D4",
        "#10B981", "#E8F0FF",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2A0E");
    t.queue_next_request = RGBA::from_hex("#0F3A3A");
    t
}

fn orng() -> Theme {
    Theme {
        background: RGBA::from_hex("#00000000"),
        background_panel: RGBA::from_hex("#00000000"),
        background_element: RGBA::from_hex("#00000000"),
        background_menu: RGBA::from_hex("#2a1a1599"),
        border: RGBA::from_hex("#EC5B2B"),
        border_active: RGBA::from_hex("#EE7948"),
        border_subtle: RGBA::from_hex("#3c3c3c"),
        text: RGBA::from_hex("#eeeeee"),
        text_muted: RGBA::from_hex("#808080"),
        selected_list_item_text: RGBA::from_hex("#0a0a0a"),
        primary: RGBA::from_hex("#FF6B30"),
        secondary: RGBA::from_hex("#EE7948"),
        accent: RGBA::from_hex("#FFF7F1"),
        success: RGBA::from_hex("#6ba1e6"),
        warning: RGBA::from_hex("#EC5B2B"),
        error: RGBA::from_hex("#e06c75"),
        info: RGBA::from_hex("#56b6c2"),
        diff_added: RGBA::from_hex("#6ba1e6"),
        diff_removed: RGBA::from_hex("#c53b53"),
        diff_context: RGBA::from_hex("#828bb8"),
        diff_hunk_header: RGBA::from_hex("#828bb8"),
        diff_highlight_added: RGBA::from_hex("#6ba1e6"),
        diff_highlight_removed: RGBA::from_hex("#e26a75"),
        diff_added_bg: RGBA::from_hex("#00000000"),
        diff_removed_bg: RGBA::from_hex("#00000000"),
        diff_context_bg: RGBA::from_hex("#00000000"),
        diff_line_number: RGBA::from_hex("#808080"),
        diff_added_line_number_bg: RGBA::from_hex("#00000000"),
        diff_removed_line_number_bg: RGBA::from_hex("#00000000"),
        markdown_text: RGBA::from_hex("#eeeeee"),
        markdown_heading: RGBA::from_hex("#EC5B2B"),
        markdown_link: RGBA::from_hex("#EC5B2B"),
        markdown_link_text: RGBA::from_hex("#56b6c2"),
        markdown_code: RGBA::from_hex("#6ba1e6"),
        markdown_block_quote: RGBA::from_hex("#FFF7F1"),
        markdown_emph: RGBA::from_hex("#e5c07b"),
        markdown_strong: RGBA::from_hex("#EE7948"),
        markdown_horizontal_rule: RGBA::from_hex("#808080"),
        markdown_list_item: RGBA::from_hex("#EC5B2B"),
        markdown_list_enumeration: RGBA::from_hex("#56b6c2"),
        markdown_image: RGBA::from_hex("#EC5B2B"),
        markdown_image_text: RGBA::from_hex("#56b6c2"),
        markdown_code_block: RGBA::from_hex("#eeeeee"),
        syntax_comment: RGBA::from_hex("#808080"),
        syntax_keyword: RGBA::from_hex("#EC5B2B"),
        syntax_function: RGBA::from_hex("#EE7948"),
        syntax_variable: RGBA::from_hex("#e06c75"),
        syntax_string: RGBA::from_hex("#6ba1e6"),
        syntax_number: RGBA::from_hex("#FFF7F1"),
        syntax_type: RGBA::from_hex("#e5c07b"),
        syntax_operator: RGBA::from_hex("#56b6c2"),
        syntax_punctuation: RGBA::from_hex("#eeeeee"),
        queue_next_loop: RGBA::from_hex("#3A1C0C"),
        queue_next_request: RGBA::from_hex("#0C2A2E"),
        thinking_opacity: 0.6,
    }
}

fn opencode() -> Theme {
    let mut t = full_theme(
        "#0a0a0a", "#141414", "#1e1e1e", "#484848", "#606060", "#3c3c3c", "#eeeeee", "#808080",
        "#fab283", "#5c9cf5", "#9d7cd8", "#7fd88f", "#f5a742", "#e06c75", "#56b6c2", "#4fd6be",
        "#c53b53", "#828bb8", "#828bb8", "#b8db87", "#e26a75", "#20303b", "#37222c", "#141414",
        "#8f8f8f", "#1b2b34", "#2d1f26", "#eeeeee", "#9d7cd8", "#fab283", "#56b6c2", "#7fd88f",
        "#e5c07b", "#e5c07b", "#f5a742", "#808080", "#fab283", "#56b6c2", "#fab283", "#56b6c2",
        "#eeeeee", "#808080", "#9d7cd8", "#fab283", "#e06c75", "#7fd88f", "#f5a742", "#e5c07b",
        "#56b6c2", "#eeeeee",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2E1A");
    t.queue_next_request = RGBA::from_hex("#122A30");
    t
}

fn tokyonight() -> Theme {
    let mut t = full_theme(
        "#1a1b26", "#1e2030", "#222436", "#737aa2", "#9099b2", "#545c7e", "#c8d3f5", "#828bb8",
        "#82aaff", "#c099ff", "#ff966c", "#c3e88d", "#ff966c", "#ff757f", "#82aaff", "#4fd6be",
        "#c53b53", "#828bb8", "#828bb8", "#b8db87", "#e26a75", "#20303b", "#37222c", "#1e2030",
        "#8f909a", "#1b2b34", "#2d1f26", "#c8d3f5", "#c099ff", "#82aaff", "#86e1fc", "#c3e88d",
        "#ffc777", "#ffc777", "#ff966c", "#828bb8", "#82aaff", "#86e1fc", "#82aaff", "#86e1fc",
        "#c8d3f5", "#828bb8", "#c099ff", "#82aaff", "#ff757f", "#c3e88d", "#ff966c", "#ffc777",
        "#86e1fc", "#c8d3f5",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2C1C");
    t.queue_next_request = RGBA::from_hex("#1A2A3A");
    t
}

fn catppuccin() -> Theme {
    let mut t = full_theme(
        "#1e1e2e", "#181825", "#11111b", "#313244", "#45475a", "#585b70", "#cdd6f4", "#9399b2",
        "#89b4fa", "#cba6f7", "#f5c2e7", "#a6e3a1", "#f9e2af", "#f38ba8", "#94e2d5", "#a6e3a1",
        "#f38ba8", "#9399b2", "#fab387", "#a6e3a1", "#f38ba8", "#24312b", "#3c2a32", "#181825",
        "#9399b2", "#1e2a25", "#32232a", "#cdd6f4", "#cba6f7", "#89b4fa", "#89dceb", "#a6e3a1",
        "#f9e2af", "#f9e2af", "#fab387", "#a6adc8", "#89b4fa", "#89dceb", "#89b4fa", "#89dceb",
        "#cdd6f4", "#9399b2", "#cba6f7", "#89b4fa", "#f38ba8", "#a6e3a1", "#fab387", "#f9e2af",
        "#89dceb", "#cdd6f4",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3622");
    t.queue_next_request = RGBA::from_hex("#233045");
    t
}

fn dracula() -> Theme {
    let mut t = full_theme(
        "#282a36", "#21222c", "#44475a", "#44475a", "#bd93f9", "#191a21", "#f8f8f2", "#6272a4",
        "#bd93f9", "#ff79c6", "#8be9fd", "#50fa7b", "#f1fa8c", "#ff5555", "#ffb86c", "#50fa7b",
        "#ff5555", "#6272a4", "#6272a4", "#50fa7b", "#ff5555", "#1a3a1a", "#3a1a1a", "#21222c",
        "#989aa4", "#1a3a1a", "#3a1a1a", "#f8f8f2", "#bd93f9", "#8be9fd", "#ff79c6", "#50fa7b",
        "#6272a4", "#f1fa8c", "#ffb86c", "#6272a4", "#bd93f9", "#8be9fd", "#8be9fd", "#ff79c6",
        "#f8f8f2", "#6272a4", "#ff79c6", "#50fa7b", "#f8f8f2", "#f1fa8c", "#bd93f9", "#8be9fd",
        "#ff79c6", "#f8f8f2",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3A22");
    t.queue_next_request = RGBA::from_hex("#1E3A3D");
    t
}

fn nord() -> Theme {
    let mut t = full_theme(
        "#2E3440", "#3B4252", "#434C5E", "#434C5E", "#4C566A", "#434C5E", "#ECEFF4", "#8B95A7",
        "#88C0D0", "#81A1C1", "#8FBCBB", "#A3BE8C", "#D08770", "#BF616A", "#88C0D0", "#A3BE8C",
        "#BF616A", "#8B95A7", "#8B95A7", "#A3BE8C", "#BF616A", "#3B4252", "#3B4252", "#3B4252",
        "#a9aeb6", "#3B4252", "#3B4252", "#D8DEE9", "#88C0D0", "#81A1C1", "#8FBCBB", "#A3BE8C",
        "#8B95A7", "#D08770", "#EBCB8B", "#8B95A7", "#88C0D0", "#8FBCBB", "#81A1C1", "#8FBCBB",
        "#D8DEE9", "#8B95A7", "#81A1C1", "#88C0D0", "#8FBCBB", "#A3BE8C", "#B48EAD", "#8FBCBB",
        "#81A1C1", "#D8DEE9",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3428");
    t.queue_next_request = RGBA::from_hex("#24323E");
    t
}

fn one_dark() -> Theme {
    let mut t = full_theme(
        "#282c34", "#21252b", "#353b45", "#393f4a", "#61afef", "#2c313a", "#abb2bf", "#5c6370",
        "#61afef", "#c678dd", "#56b6c2", "#98c379", "#e5c07b", "#e06c75", "#d19a66", "#98c379",
        "#e06c75", "#5c6370", "#56b6c2", "#aad482", "#e8828b", "#2c382b", "#3a2d2f", "#21252b",
        "#9398a2", "#283427", "#36292b", "#abb2bf", "#c678dd", "#61afef", "#56b6c2", "#98c379",
        "#5c6370", "#e5c07b", "#d19a66", "#5c6370", "#61afef", "#56b6c2", "#61afef", "#56b6c2",
        "#abb2bf", "#5c6370", "#c678dd", "#61afef", "#e06c75", "#98c379", "#d19a66", "#e5c07b",
        "#56b6c2", "#abb2bf",
    );
    t.queue_next_loop = RGBA::from_hex("#3A341E");
    t.queue_next_request = RGBA::from_hex("#183037");
    t
}

fn gruvbox() -> Theme {
    let mut t = full_theme(
        "#282828", "#3c3836", "#504945", "#665c54", "#ebdbb2", "#504945", "#ebdbb2", "#928374",
        "#83a598", "#d3869b", "#8ec07c", "#b8bb26", "#fe8019", "#fb4934", "#fabd2f", "#98971a",
        "#cc241d", "#928374", "#689d6a", "#b8bb26", "#fb4934", "#32302f", "#322929", "#3c3836",
        "#a8a29e", "#2a2827", "#2a2222", "#ebdbb2", "#83a598", "#8ec07c", "#b8bb26", "#fabd2f",
        "#928374", "#d3869b", "#fe8019", "#928374", "#83a598", "#8ec07c", "#8ec07c", "#b8bb26",
        "#ebdbb2", "#928374", "#fb4934", "#b8bb26", "#83a598", "#fabd2f", "#d3869b", "#8ec07c",
        "#fe8019", "#ebdbb2",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2A14");
    t.queue_next_request = RGBA::from_hex("#1F3136");
    t
}

fn solarized() -> Theme {
    let mut t = full_theme(
        "#002b36", "#073642", "#073642", "#073642", "#586e75", "#073642", "#839496", "#586e75",
        "#268bd2", "#6c71c4", "#2aa198", "#859900", "#b58900", "#dc322f", "#cb4b16", "#859900",
        "#dc322f", "#586e75", "#586e75", "#859900", "#dc322f", "#073642", "#073642", "#073642",
        "#8b9b9f", "#073642", "#073642", "#839496", "#268bd2", "#2aa198", "#6c71c4", "#859900",
        "#586e75", "#b58900", "#cb4b16", "#586e75", "#268bd2", "#2aa198", "#2aa198", "#6c71c4",
        "#839496", "#586e75", "#859900", "#268bd2", "#2aa198", "#2aa198", "#d33682", "#b58900",
        "#859900", "#839496",
    );
    t.queue_next_loop = RGBA::from_hex("#2E2A10");
    t.queue_next_request = RGBA::from_hex("#0E3A35");
    t
}

fn monokai() -> Theme {
    let mut t = full_theme(
        "#272822", "#1e1f1c", "#3e3d32", "#3e3d32", "#66d9ef", "#1e1f1c", "#f8f8f2", "#75715e",
        "#66d9ef", "#ae81ff", "#a6e22e", "#a6e22e", "#e6db74", "#f92672", "#fd971f", "#a6e22e",
        "#f92672", "#75715e", "#75715e", "#a6e22e", "#f92672", "#1a3a1a", "#3a1a1a", "#1e1f1c",
        "#9b9b95", "#1a3a1a", "#3a1a1a", "#f8f8f2", "#f92672", "#66d9ef", "#ae81ff", "#a6e22e",
        "#75715e", "#e6db74", "#fd971f", "#75715e", "#66d9ef", "#ae81ff", "#66d9ef", "#ae81ff",
        "#f8f8f2", "#75715e", "#f92672", "#a6e22e", "#f8f8f2", "#e6db74", "#ae81ff", "#66d9ef",
        "#f92672", "#f8f8f2",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3318");
    t.queue_next_request = RGBA::from_hex("#123A40");
    t
}

fn everforest() -> Theme {
    let mut t = full_theme(
        "#2d353b", "#333c43", "#343f44", "#859289", "#9da9a0", "#7a8478", "#d3c6aa", "#7a8478",
        "#a7c080", "#7fbbb3", "#d699b6", "#a7c080", "#e69875", "#e67e80", "#83c092", "#4fd6be",
        "#c53b53", "#828bb8", "#828bb8", "#b8db87", "#e26a75", "#20303b", "#37222c", "#333c43",
        "#a0a5a7", "#1b2b34", "#2d1f26", "#d3c6aa", "#d699b6", "#a7c080", "#83c092", "#a7c080",
        "#dbbc7f", "#dbbc7f", "#e69875", "#7a8478", "#a7c080", "#83c092", "#a7c080", "#83c092",
        "#d3c6aa", "#7a8478", "#d699b6", "#a7c080", "#e67e80", "#a7c080", "#e69875", "#dbbc7f",
        "#83c092", "#d3c6aa",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3026");
    t.queue_next_request = RGBA::from_hex("#1E362E");
    t
}

fn kanagawa() -> Theme {
    let mut t = full_theme(
        "#1F1F28", "#2A2A37", "#363646", "#54546D", "#C38D9D", "#363646", "#DCD7BA", "#727169",
        "#7E9CD8", "#957FB8", "#D27E99", "#98BB6C", "#D7A657", "#E82424", "#76946A", "#98BB6C",
        "#E82424", "#727169", "#2D4F67", "#A9D977", "#F24A4A", "#252E25", "#362020", "#2A2A37",
        "#9090a0", "#202820", "#2D1C1C", "#DCD7BA", "#957FB8", "#7E9CD8", "#76946A", "#98BB6C",
        "#727169", "#C38D9D", "#D7A657", "#727169", "#7E9CD8", "#76946A", "#7E9CD8", "#76946A",
        "#DCD7BA", "#727169", "#957FB8", "#7E9CD8", "#DCD7BA", "#98BB6C", "#D7A657", "#C38D9D",
        "#D27E99", "#DCD7BA",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3120");
    t.queue_next_request = RGBA::from_hex("#242E40");
    t
}

fn rosepine() -> Theme {
    let mut t = full_theme(
        "#191724", "#1f1d2e", "#26233a", "#403d52", "#9ccfd8", "#21202e", "#e0def4", "#6e6a86",
        "#9ccfd8", "#c4a7e7", "#ebbcba", "#31748f", "#f6c177", "#eb6f92", "#9ccfd8", "#31748f",
        "#eb6f92", "#6e6a86", "#c4a7e7", "#31748f", "#eb6f92", "#1f2d3a", "#3a1f2d", "#1f1d2e",
        "#9491a6", "#1f2d3a", "#3a1f2d", "#e0def4", "#c4a7e7", "#9ccfd8", "#ebbcba", "#31748f",
        "#6e6a86", "#f6c177", "#eb6f92", "#403d52", "#9ccfd8", "#ebbcba", "#9ccfd8", "#ebbcba",
        "#e0def4", "#6e6a86", "#31748f", "#ebbcba", "#e0def4", "#f6c177", "#c4a7e7", "#9ccfd8",
        "#908caa", "#908caa",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3020");
    t.queue_next_request = RGBA::from_hex("#22303A");
    t
}

fn github() -> Theme {
    let mut t = full_theme(
        "#0d1117", "#010409", "#161b22", "#30363d", "#58a6ff", "#21262d", "#c9d1d9", "#8b949e",
        "#58a6ff", "#bc8cff", "#39c5cf", "#3fb950", "#e3b341", "#f85149", "#d29922", "#3fb950",
        "#f85149", "#8b949e", "#58a6ff", "#3fb950", "#f85149", "#033a16", "#67060c", "#010409",
        "#95999e", "#033a16", "#67060c", "#c9d1d9", "#58a6ff", "#58a6ff", "#39c5cf", "#ff7b72",
        "#8b949e", "#e3b341", "#d29922", "#30363d", "#58a6ff", "#39c5cf", "#58a6ff", "#39c5cf",
        "#c9d1d9", "#8b949e", "#ff7b72", "#bc8cff", "#d29922", "#39c5cf", "#58a6ff", "#d29922",
        "#ff7b72", "#c9d1d9",
    );
    t.queue_next_loop = RGBA::from_hex("#3A3018");
    t.queue_next_request = RGBA::from_hex("#13283A");
    t
}

fn ayu() -> Theme {
    let mut t = full_theme(
        "#0B0E14", "#0F131A", "#0D1017", "#6C7380", "#6C7380", "#11151C", "#BFBDB6", "#565B66",
        "#59C2FF", "#D2A6FF", "#E6B450", "#7FD962", "#E6B673", "#D95757", "#39BAE6", "#7FD962",
        "#F26D78", "#ACB6BF", "#ACB6BF", "#AAD94C", "#F07178", "#20303b", "#37222c", "#0F131A",
        "#ACB6BF", "#1b2b34", "#2d1f26", "#BFBDB6", "#D2A6FF", "#59C2FF", "#39BAE6", "#AAD94C",
        "#E6B673", "#E6B673", "#FFB454", "#565B66", "#59C2FF", "#39BAE6", "#59C2FF", "#39BAE6",
        "#BFBDB6", "#ACB6BF", "#FF8F40", "#FFB454", "#59C2FF", "#AAD94C", "#D2A6FF", "#E6B673",
        "#F29668", "#BFBDB6",
    );
    t.queue_next_loop = RGBA::from_hex("#3A2E1C");
    t.queue_next_request = RGBA::from_hex("#123040");
    t
}

fn material() -> Theme {
    let mut t = full_theme(
        "#263238", "#1e272c", "#37474f", "#37474f", "#82aaff", "#1e272c", "#eeffff", "#546e7a",
        "#82aaff", "#c792ea", "#89ddff", "#c3e88d", "#ffcb6b", "#f07178", "#ffcb6b", "#c3e88d",
        "#f07178", "#546e7a", "#89ddff", "#c3e88d", "#f07178", "#2e3c2b", "#3c2b2b", "#1e272c",
        "#9aa2a6", "#2e3c2b", "#3c2b2b", "#eeffff", "#82aaff", "#89ddff", "#c792ea", "#c3e88d",
        "#546e7a", "#ffcb6b", "#ffcb6b", "#37474f", "#82aaff", "#89ddff", "#89ddff", "#c792ea",
        "#eeffff", "#546e7a", "#c792ea", "#82aaff", "#eeffff", "#c3e88d", "#ffcb6b", "#ffcb6b",
        "#89ddff", "#eeffff",
    );
    t.queue_next_loop = RGBA::from_hex("#3A331C");
    t.queue_next_request = RGBA::from_hex("#1A2E3A");
    t
}
