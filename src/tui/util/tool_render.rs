use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use cosh_sdk::tree_sitter::highlight::{HighlightCategory, highlight};
use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::diff::DiffRenderable;

use crate::component::spinner::SpinnerState;
use crate::theme::Theme;
use crate::types::{ToolPart, ToolStatus};

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

fn lang_name_from_path(filepath: &str) -> Option<&'static str> {
    let ext = Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "hs" | "lhs" => "haskell",
        "swift" => "swift",
        "zig" | "zon" => "zig",
        "kt" | "kts" => "kotlin",
        _ => return None,
    })
}

fn code_highlight_style(cat: Option<HighlightCategory>, default_fg: Color) -> Style {
    let fg = match cat {
        Some(HighlightCategory::Keyword) => Color::Rgb(255, 180, 100),
        Some(HighlightCategory::String) => Color::Rgb(150, 200, 150),
        Some(HighlightCategory::Comment) => Color::Rgb(130, 130, 140),
        Some(HighlightCategory::Type) => Color::Rgb(100, 180, 255),
        Some(HighlightCategory::Function) => Color::Rgb(200, 180, 255),
        Some(HighlightCategory::Number) => Color::Rgb(255, 200, 100),
        Some(HighlightCategory::Builtin) => Color::Rgb(100, 200, 255),
        None => default_fg,
    };
    Style::default().fg(fg)
}

#[allow(clippy::too_many_arguments)]
fn draw_highlighted_code(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    content: &str,
    lang: Option<&str>,
    default_fg: Color,
    max_lines: u16,
) -> u16 {
    let Some(lang) = lang else {
        // Fallback to plain text
        let style = Style::default().fg(default_fg);
        for (i, line) in content.lines().enumerate().take(max_lines as usize) {
            draw_text_line(buf, line, x, y + i as u16, max_w, style);
        }
        return content.lines().count().min(max_lines as usize) as u16;
    };

    let spans = highlight(content, lang);
    let mut cat_map: Vec<Option<HighlightCategory>> = vec![None; content.len()];

    if let Some(ref spans) = spans {
        for span in spans {
            for item in &mut cat_map[span.start..span.end.min(content.len())] {
                *item = Some(span.category);
            }
        }
    }

    let mut y_pos = y;
    let mut byte_offset = 0;
    let mut lines_drawn = 0u16;

    for (i, line) in content.lines().enumerate() {
        if lines_drawn >= max_lines {
            break;
        }
        if i > 0 {
            y_pos += 1;
        }

        for (x_pos, (ci, ch)) in (x..).zip(line.char_indices()) {
            if x_pos >= x + max_w {
                break;
            }
            let byte_pos = byte_offset + ci;
            let cat = cat_map.get(byte_pos).copied().flatten();
            let style = code_highlight_style(cat, default_fg);
            if let Some(cell) = buf.cell_mut((x_pos, y_pos)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
        byte_offset += line.len() + 1;
        lines_drawn += 1;
    }

    lines_drawn
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

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
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
        draw_text_line(
            buf,
            &spinner.current_char().to_string(),
            x,
            y,
            1,
            icon_style,
        );
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
        Self {
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

    pub const fn advance_spinner(&mut self) {
        self.spinner.advance();
    }
}

impl Default for ToolRenderState {
    fn default() -> Self {
        Self::new()
    }
}

const TOOL_DISPLAYS: &[&str] = &[
    "bash",
    "glob",
    "read",
    "grep",
    "webfetch",
    "websearch",
    "write",
    "edit",
    "task",
    "apply_patch",
    "question",
    "skill",
];

pub fn tool_display(tool: &str) -> &str {
    if TOOL_DISPLAYS.contains(&tool) {
        return tool;
    }
    if tool.starts_with("plan_") {
        return "todo";
    }
    "generic"
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

#[allow(clippy::too_many_arguments)]
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

    if output.is_empty() {
        let icon = "$";
        let pending = "Writing command...";
        let label = if is_completed { &command } else { pending };
        let fg = if is_completed {
            theme.text_muted
        } else if is_running {
            theme.text
        } else {
            theme.text_muted
        };
        *line_h = 1;
        render_inline_tool(
            buf,
            x,
            y,
            max_w,
            icon,
            label,
            fg,
            None,
            false,
            false,
            if is_running {
                Some(&state.spinner)
            } else {
                None
            },
            false,
            false,
        );
    } else {
        let expanded = state.is_expanded(id);
        let collapsed = crate::util::scroll::collapse_tool_output(&output, 10, 800);
        let display = if expanded || !collapsed.overflow {
            &output
        } else {
            &collapsed.output
        };

        let title = format!("$ {command}");
        let lines = display.lines().count() as u16 + u16::from(collapsed.overflow);
        let area = Rect::new(x, y, max_w.saturating_add(3), lines + 2);
        *line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(buf, area);

        let x_off = x + 3;
        let title_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &title, x_off, y, max_w.saturating_sub(3), title_style);

        let content_style = Style::default().fg(rgba_color(theme.text));
        for (i, line) in display.lines().enumerate() {
            let ly = y + 1 + i as u16;
            if ly >= area.bottom() {
                break;
            }
            draw_text_line(buf, line, x_off, ly, max_w.saturating_sub(3), content_style);
        }
        if collapsed.overflow {
            let hint_y = y + 1 + display.lines().count() as u16;
            let hint = if expanded {
                "Click to collapse"
            } else {
                "Click to expand"
            };
            draw_text_line(
                buf,
                hint,
                x_off,
                hint_y,
                max_w.saturating_sub(3),
                Style::default().fg(rgba_color(theme.text_muted)),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
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
        let max_lines = 20u16;
        let lines = content.lines().count() as u16;
        let area = Rect::new(x, y, max_w.saturating_add(3), lines.min(max_lines) + 2);
        *line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(buf, area);

        let title = format!("# Wrote {filepath}");
        let title_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &title, x + 3, y, max_w.saturating_sub(3), title_style);

        let max_w_inner = max_w.saturating_sub(3);
        let default_fg = rgba_color(theme.text);
        let lang = lang_name_from_path(&filepath);
        draw_highlighted_code(
            buf,
            x + 3,
            y + 1,
            max_w_inner,
            &content,
            lang,
            default_fg,
            max_lines,
        );
    } else {
        let icon = "\u{2190}";
        let label = format!("Write {filepath}");
        let fg = if is_completed {
            theme.text_muted
        } else {
            theme.text
        };
        *line_h = 1;
        render_inline_tool(
            buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
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
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
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
        let fg = if is_completed {
            theme.text_muted
        } else {
            theme.text
        };
        *line_h = 1;
        render_inline_tool(
            buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
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
        let _ = write!(label, " in {p}");
    }

    let icon = "\u{2731}";
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
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
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf,
        x,
        y,
        max_w,
        icon,
        &label,
        fg,
        None,
        false,
        false,
        if is_running {
            Some(&state.spinner)
        } else {
            None
        },
        false,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
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
        let _ = write!(label, " in {p}");
    }

    let icon = "\u{2731}";
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
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
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
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
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
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
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf,
        x,
        y,
        max_w,
        icon,
        &content,
        fg,
        None,
        false,
        false,
        if is_running && !is_completed {
            Some(&state.spinner)
        } else {
            None
        },
        false,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
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
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn render_todo(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_h: &mut u16,
    max_w: u16,
    part: &ToolPart,
    _state: &ToolRenderState,
    theme: &Theme,
) {
    let tool_name: &str = &part.tool;
    let output = part.output.as_deref().unwrap_or("").trim().to_string();
    let is_running = matches!(part.status, ToolStatus::Running);

    if is_running || output.is_empty() {
        let icon = "\u{2630}";
        let label = format!("Writing {tool_name}...");
        let fg = theme.text;
        *line_h = 1;
        render_inline_tool(
            buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
        );
        return;
    }

    let mut lines: Vec<String> = Vec::new();

    lines.push(match tool_name {
        "plan_todo_write" => "\u{270F} TODO Write",
        "plan_todo_edit" => "\u{270F} TODO Edit",
        "plan_todo_cross_off" => "\u{2713} TODO Cross Off",
        "plan_todo_read" => "\u{2630} TODO Read",
        "plan_load_from_md" => "\u{1F4C2} TODO Load",
        _ => "\u{2630} TODO",
    }
    .to_string());

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&output) {
        let groups = json
            .get("list")
            .and_then(|l| l.get("groups"))
            .or_else(|| json.get("groups"))
            .and_then(|g| g.as_array());

        if let Some(groups) = groups {
            for group in groups {
                let title = group
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Untitled");
                lines.push(format!("  {title}:"));
                if let Some(items) = group.get("items").and_then(|i| i.as_array()) {
                    for item in items {
                        let id = item.get("id").and_then(|i| i.as_str()).unwrap_or("?");
                        let desc = item
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("");
                        let status = item
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("?");
                        let icon = match status {
                            "Completed" => "\u{2713}",
                            "InProgress" => "\u{25CF}",
                            "Cancelled" => "\u{2717}",
                            _ => "\u{25CB}",
                        };
                        lines.push(format!("    {icon} {desc}  ({id})"));
                    }
                }
            }
        }

        if let Some(nags) = json.get("nags").and_then(|n| n.as_array()) {
            for nag in nags {
                if let Some(msg) = nag.get("message").and_then(|m| m.as_str()) {
                    lines.push(format!("  \u{26A0} {msg}"));
                }
            }
        }

        if groups.is_none() && json.get("nags").and_then(|n| n.as_array()).map_or(true, |n| n.is_empty())
        {
            lines.push("  (empty)".to_string());
        }
    } else {
        for line in output.lines() {
            lines.push(line.to_string());
        }
    }

    let display = lines.join("\n");
    let line_count = display.lines().count() as u16;
    let area = Rect::new(x, y, max_w.saturating_add(3), line_count.saturating_add(2));
    *line_h = area.height;

    let mut border_box = BoxRenderable::new();
    border_box.set_background_color(Some(theme.background_panel.into()));
    border_box.set_border_color(Some(theme.background.into()));
    border_box.set_border_sides(BorderSidesConfig {
        left: true,
        top: false,
        right: false,
        bottom: false,
    });
    border_box.set_custom_border_chars(BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '\u{2503}',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '\u{2503}',
        right_t: ' ',
        cross: ' ',
    });
    border_box.render_self(buf, area);

    let title_style = Style::default().fg(rgba_color(theme.text_muted));
    let title = &lines[0];
    draw_text_line(buf, title, x + 3, y, max_w.saturating_sub(3), title_style);

    let content_style = Style::default().fg(rgba_color(theme.text));
    for (i, line) in display.lines().enumerate().skip(1) {
        let ly = y + 1 + i as u16;
        if ly >= area.bottom() {
            break;
        }
        draw_text_line(buf, line, x + 3, ly, max_w.saturating_sub(3), content_style);
    }
}

#[allow(clippy::too_many_arguments)]
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
        tool_name.clone()
    } else {
        format!("Writing {tool_name}...")
    };
    let icon = "\u{2699}";
    let fg = if is_completed {
        theme.text_muted
    } else {
        theme.text
    };
    *line_h = 1;
    render_inline_tool(
        buf, x, y, max_w, icon, &label, fg, None, false, false, None, false, false,
    );
}

#[allow(clippy::too_many_arguments)]
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
        "todo" => render_todo(buf, x, y, line_h, max_w, part, state, theme),
        _ => render_generic(buf, x, y, line_h, max_w, part, state, theme),
    }
}
