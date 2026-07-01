use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use std::collections::HashMap;

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::diff::DiffRenderable;
use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};

use crate::theme::Theme;
use crate::types::*;
use crate::component::spinner::SpinnerState;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn pad_right(text: &str, width: usize) -> String {
    let len: usize = text.chars().count();
    if len >= width {
        text.chars().take(width).collect()
    } else {
        let mut s = text.to_string();
        s.push_str(&" ".repeat(width - len));
        s
    }
}

fn render_inline_tool(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    icon: &str,
    text: &str,
    fg: RGBA,
    _error_fg: Option<RGBA>,
    _failed: bool,
    _denied: bool,
    spinner: Option<&SpinnerState>,
    _expandable: bool,
    _expanded: bool,
) {
    let base_style = Style::default().fg(rgba_color(fg));
    let icon_style = Style::default().fg(rgba_color(fg));

    if let Some(spinner) = spinner {
        draw_text_line(buf, &spinner.current_char().to_string(), x, y, 1, icon_style);
        draw_text_line(buf, " ", x + 1, y, 1, icon_style);
        let label_x = x + 2;
        draw_text_line(buf, text, label_x, y, max_w.saturating_sub(2), base_style);
    } else if icon.len() <= 2 {
        let padded = pad_right(icon, 2);
        draw_text_line(buf, &padded, x, y, 2, icon_style);
        let label_x = x + 2;
        draw_text_line(buf, text, label_x, y, max_w.saturating_sub(2), base_style);
    } else {
        draw_text_line(buf, icon, x, y, max_w, icon_style);
    }
}

pub struct ToolRenderState {
    pub expanded: HashMap<String, bool>,
    pub error_expanded: HashMap<String, bool>,
    pub spinner: SpinnerState,
}

impl ToolRenderState {
    pub fn new() -> Self {
        ToolRenderState {
            expanded: HashMap::new(),
            error_expanded: HashMap::new(),
            spinner: SpinnerState::new(),
        }
    }

    pub fn toggle_expanded(&mut self, id: &str) {
        let entry = self.expanded.entry(id.to_string()).or_insert(false);
        *entry = !*entry;
    }

    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.get(id).copied().unwrap_or(false)
    }

    pub fn toggle_error(&mut self, id: &str) {
        let entry = self.error_expanded.entry(id.to_string()).or_insert(false);
        *entry = !*entry;
    }

    pub fn advance_spinner(&mut self) {
        self.spinner.advance();
    }
}

impl Default for ToolRenderState {
    fn default() -> Self {
        Self::new()
    }
}

const TOOL_DISPLAYS: &[&str] = &[
    "bash", "glob", "read", "grep", "webfetch", "websearch",
    "write", "edit", "task", "apply_patch", "todowrite", "question", "skill",
];

pub fn tool_display(tool: &str) -> &str {
    if TOOL_DISPLAYS.contains(&tool) { tool } else { "generic" }
}

pub fn web_search_provider_label(provider: Option<&str>) -> &str {
    match provider {
        Some("parallel") => "Parallel Web Search",
        Some("exa") => "Exa Web Search",
        _ => "Web Search",
    }
}

fn input_value(input: &serde_json::Value, key: &str) -> Option<String> {
    input.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

pub fn render_shell(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    state: &ToolRenderState,
    theme: &Theme,
) {
    let command = input_value(&part.input, "command").unwrap_or_default();
    let output = part.output.as_deref().unwrap_or("").trim().to_string();
    let id = part.tool_call_id.as_deref().unwrap_or("shell");
    let is_running = matches!(part.status, ToolStatus::Running);
    let is_completed = matches!(part.status, ToolStatus::Completed);

    if !output.is_empty() {
        let expanded = state.is_expanded(id);
        let collapsed = crate::util::scroll::collapse_tool_output(&output, 10, 800);
        let display = if expanded || !collapsed.overflow { &output } else { &collapsed.output };

        let title = format!("$ {command}");
        let lines = display.lines().count() as u16 + if collapsed.overflow { 1 } else { 0 };
        let area = Rect::new(x, y, max_w.saturating_add(3), lines + 2);
        *line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig { left: true, top: false, right: false, bottom: false });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ', top_right: ' ', bottom_left: ' ', bottom_right: ' ',
            horizontal: ' ', vertical: '┃', top_t: ' ', bottom_t: ' ',
            left_t: '┃', right_t: ' ', cross: ' ',
        });
        border_box.render_self(buf, area);

        let x_off = x + 3;
        let title_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &title, x_off, y, max_w.saturating_sub(3), title_style);

        let content_style = Style::default().fg(rgba_color(theme.text));
        for (i, line) in display.lines().enumerate() {
            let ly = y + 1 + i as u16;
            if ly >= area.bottom() { break; }
            draw_text_line(buf, line, x_off, ly, max_w.saturating_sub(3), content_style);
        }
        if collapsed.overflow {
            let hint_y = y + 1 + display.lines().count() as u16;
            let hint = if expanded { "Click to collapse" } else { "Click to expand" };
            draw_text_line(buf, hint, x_off, hint_y, max_w.saturating_sub(3), Style::default().fg(rgba_color(theme.text_muted)));
        }
    } else {
        let icon = "$";
        let pending = "Writing command...";
        let label = if is_completed { &command } else { pending };
        let fg = if is_completed { theme.text_muted } else if is_running { theme.text } else { theme.text_muted };
        *line_h = 1;
        render_inline_tool(buf, x, y, max_w, icon, label, fg, None, false, false,
            if is_running { Some(&state.spinner) } else { None }, false, false);
    }
}

pub fn render_write(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let filepath = input_value(&part.input, "filePath").unwrap_or_default();
    let content = input_value(&part.input, "content").unwrap_or_default();
    let is_completed = matches!(part.status, ToolStatus::Completed);

    if is_completed && !content.is_empty() {
        let lines = content.lines().count() as u16;
        let area = Rect::new(x, y, max_w.saturating_add(3), lines.min(20) + 2);
        *line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig { left: true, top: false, right: false, bottom: false });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ', top_right: ' ', bottom_left: ' ', bottom_right: ' ',
            horizontal: ' ', vertical: '┃', top_t: ' ', bottom_t: ' ',
            left_t: '┃', right_t: ' ', cross: ' ',
        });
        border_box.render_self(buf, area);

        let title = format!("# Wrote {filepath}");
        let title_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &title, x + 3, y, max_w.saturating_sub(3), title_style);

        let content_style = Style::default().fg(rgba_color(theme.text));
        for (i, line) in content.lines().enumerate().take(20) {
            let ly = y + 1 + i as u16;
            if ly >= area.bottom() { break; }
            draw_text_line(buf, line, x + 3, ly, max_w.saturating_sub(3), content_style);
        }
    } else {
        let icon = "\u{2190}";
        let label = format!("Write {filepath}");
        let fg = if is_completed { theme.text_muted } else { theme.text };
        *line_h = 1;
        render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
    }
}

pub fn render_edit(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let filepath = input_value(&part.input, "filePath").unwrap_or_default();
    let diff_content = part.output.as_deref().unwrap_or("").to_string();
    let is_completed = matches!(part.status, ToolStatus::Completed);

    if is_completed && !diff_content.is_empty() {
        let diff_lines = diff_content.lines().count() as u16;
        let area = Rect::new(x, y, max_w.saturating_add(3), diff_lines.min(30) + 2);
        *line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig { left: true, top: false, right: false, bottom: false });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ', top_right: ' ', bottom_left: ' ', bottom_right: ' ',
            horizontal: ' ', vertical: '┃', top_t: ' ', bottom_t: ' ',
            left_t: '┃', right_t: ' ', cross: ' ',
        });
        border_box.render_self(buf, area);

        let title = format!("\u{2190} Edit {filepath}");
        let title_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &title, x + 3, y, max_w.saturating_sub(3), title_style);

        let diff_area = Rect::new(x + 3, y + 1, max_w.saturating_sub(3), diff_lines.min(30));
        let diff = DiffRenderable::new(Some(diff_content));
        diff.render_self(buf, diff_area);
    } else {
        let icon = "\u{2190}";
        let label = format!("Edit {filepath}");
        let fg = if is_completed { theme.text_muted } else { theme.text };
        *line_h = 1;
        render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
    }
}

pub fn render_glob(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let pattern = input_value(&part.input, "pattern").unwrap_or_default();
    let path = input_value(&part.input, "path");
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let mut label = format!("Glob \"{pattern}\"");
    if let Some(p) = path {
        label.push_str(&format!(" in {p}"));
    }

    let icon = "\u{2731}";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn render_read(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    state: &ToolRenderState,
    theme: &Theme,
) {
    let filepath = input_value(&part.input, "filePath").unwrap_or_default();
    let is_running = matches!(part.status, ToolStatus::Running);
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let icon = "\u{2192}";
    let label = format!("Read {filepath}");
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false,
        if is_running { Some(&state.spinner) } else { None }, false, false);
}

pub fn render_grep(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let pattern = input_value(&part.input, "pattern").unwrap_or_default();
    let path = input_value(&part.input, "path");
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let mut label = format!("Grep \"{pattern}\"");
    if let Some(p) = path {
        label.push_str(&format!(" in {p}"));
    }

    let icon = "\u{2731}";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn render_webfetch(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let url = input_value(&part.input, "url").unwrap_or_default();
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let label = format!("WebFetch {url}");
    let icon = "%";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn render_websearch(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let query = input_value(&part.input, "query").unwrap_or_default();
    let provider = input_value(&part.input, "provider");
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let provider_label = web_search_provider_label(provider.as_deref());
    let label = format!("{provider_label} \"{query}\"");
    let icon = "\u{25c8}";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn render_task(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    state: &ToolRenderState,
    theme: &Theme,
) {
    let description = input_value(&part.input, "description").unwrap_or_default();
    let is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let content = if description.is_empty() {
        "Delegating...".to_string()
    } else {
        description
    };

    let icon = if is_completed { "\u{2713}" } else { "\u{2502}" };
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &content, fg, None, false, false,
        if is_running && !is_completed { Some(&state.spinner) } else { None }, false, false);
}

pub fn render_question_tool(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let label = "Asking questions...".to_string();
    let icon = "\u{2192}";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn render_generic(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let tool_name = &part.tool;
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let label = if is_completed {
        format!("{tool_name}")
    } else {
        format!("Writing {tool_name}...")
    };
    let icon = "\u{2699}";
    let fg = if is_completed { theme.text_muted } else { theme.text };
    *line_h = 1;
    render_inline_tool(buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false);
}

pub fn dispatch_tool(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    state: &ToolRenderState,
    theme: &Theme,
) {
    let display = tool_display(&part.tool);
    match display {
        "bash" => render_shell(buf, x, y, line_h, max_w, part, state, theme),
        "glob" => render_glob(buf, x, y, line_h, max_w, part, state, theme),
        "read" => render_read(buf, x, y, line_h, max_w, part, state, theme),
        "grep" => render_grep(buf, x, y, line_h, max_w, part, state, theme),
        "webfetch" => render_webfetch(buf, x, y, line_h, max_w, part, state, theme),
        "websearch" => render_websearch(buf, x, y, line_h, max_w, part, state, theme),
        "write" => render_write(buf, x, y, line_h, max_w, part, state, theme),
        "edit" => render_edit(buf, x, y, line_h, max_w, part, state, theme),
        "task" => render_task(buf, x, y, line_h, max_w, part, state, theme),
        "question" => render_question_tool(buf, x, y, line_h, max_w, part, state, theme),
        _ => render_generic(buf, x, y, line_h, max_w, part, state, theme),
    }
}
