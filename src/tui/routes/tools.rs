use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;
use crate::util::list_selection::ListSelection;

pub fn load_disabled_tools(setup: &crate::util::setup::Setup) -> HashSet<String> {
    setup.tools.disabled.iter().cloned().collect()
}

pub fn save_disabled_tools(setup: &mut crate::util::setup::Setup, disabled: &HashSet<String>) {
    setup.tools.disabled = disabled.iter().cloned().collect();
    setup.save();
}

/// Load the persisted tool-call mode. Unknown/missing values fall back to
/// `Native` (the default contract).
#[must_use]
pub fn load_tool_call_mode(setup: &crate::util::setup::Setup) -> cosh_sdk::connector::ToolCallMode {
    match setup.tools.tool_call_mode.as_str() {
        "inline" => cosh_sdk::connector::ToolCallMode::Inline,
        _ => cosh_sdk::connector::ToolCallMode::Native,
    }
}

pub fn save_tool_call_mode(
    setup: &mut crate::util::setup::Setup,
    mode: cosh_sdk::connector::ToolCallMode,
) {
    setup.tools.tool_call_mode = match mode {
        cosh_sdk::connector::ToolCallMode::Native => "native".to_string(),
        cosh_sdk::connector::ToolCallMode::Inline => "inline".to_string(),
    };
    setup.save();
}

fn internal_tools() -> &'static [(&'static str, &'static str)] {
    &[
        ("bash_run", "Execute shell commands"),
        ("fs_read", "Read file contents"),
        ("fs_write", "Write file contents"),
        ("fs_edit", "Edit file contents"),
        ("fs_rollback", "Rollback file changes"),
        ("find_glob", "Find files by glob pattern"),
        ("find_grep", "Search file contents"),
        ("web_fetch", "Fetch web page content"),
        ("web_search", "Search the web"),
        ("plan_todo_write", "Write todo items"),
        ("plan_todo_edit", "Edit todo items"),
        ("plan_todo_cross_off", "Cross off todo items"),
        ("plan_todo_read", "Read todo items"),
        ("plan_load_from_md", "Load plan from markdown"),
        ("ask_questions", "Ask the user questions"),
        #[cfg(feature = "embed")]
        (
            "recall_search",
            "Search knowledge bases for semantically similar entries",
        ),
        ("skills_list", "List available skills"),
        ("skills_read", "Read a skill"),
        ("skills_read_asset", "Read a skill asset"),
        ("skills_match_skills", "Match skills to task"),
        (
            "lsp",
            "Language-server tools (diagnostics, definitions, references, symbols, hover, rename…)",
        ),
    ]
}

pub struct InternalToolsView {
    pub selection: ListSelection,
    pub disabled: HashSet<String>,
    pub changed: bool,
}

impl InternalToolsView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            disabled: HashSet::new(),
            changed: false,
        }
    }

    pub fn select_next(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_next(internal_tools().len());
    }

    pub fn select_prev(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_prev(internal_tools().len());
    }

    pub fn toggle_current(&mut self) {
        let (name, _) = internal_tools()[self.selection.selected_index];
        if !self.disabled.remove(name) {
            self.disabled.insert(name.to_string());
        }
        self.changed = true;
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let my = mouse.y;
        let mx = mouse.x;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }
        let list_start_y = content_start_y(area) + 2;
        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let visible_count = visible_items(area);
        let hit = mx >= row_x && mx < row_x + max_row_w as u16;
        if !hit {
            return None;
        }
        for i in 0..visible_count {
            let idx = self.selection.scroll_offset + i;
            if idx >= internal_tools().len() {
                break;
            }
            let item_y = list_start_y + i as u16;
            if my == item_y {
                return Some(idx);
            }
        }
        None
    }

    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        self.find_row_for_mouse(mouse, area)
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        // title
        let title = "Internal Tools";
        let count = visible_items(area).min(internal_tools().len());
        let start_y = content_start_y(area);

        let max_w = max_row_width();
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        let title_x = row_x;
        for (i, ch) in title.chars().enumerate() {
            let cx = title_x + i as u16;
            if cx >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, start_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(muted));
            }
        }

        // Clamp selection and scroll before rendering
        self.selection.set_visible_count(count);
        self.selection.clamp(internal_tools().len());

        // tools list aligned
        let list_start_y = start_y + 2;
        if max_w == 0 {
            return;
        }

        // "  " column for the symbol (ZERO WIDTH prefix so column 0 = symbol)
        let sym_x = row_x;
        let name_x = row_x + 2; // symbol + 1 space

        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
            if idx >= internal_tools().len() {
                break;
            }
            let (name, desc) = internal_tools()[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let enabled = !self.disabled.contains(name);
            let symbol = if enabled { "✔" } else { "✗" };
            let sym_color = if enabled { Color::Green } else { Color::Red };
            let is_selected = idx == self.selection.selected_index;
            let row_color = if is_selected { primary } else { fg };

            // Symbol (✔ / ✗)
            let sym_ch = symbol.chars().next().unwrap();
            if let Some(cell) = buf.cell_mut((sym_x, y)) {
                cell.set_char(sym_ch);
                cell.set_style(Style::default().fg(sym_color));
            }

            // Name + " - " + description
            let rest = format!("{name} - {desc}");
            for (j, ch) in rest.chars().enumerate() {
                let cx = name_x + j as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(row_color));
                }
            }
        }
    }
}

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Compute the Y position where title and list start (vertically centered).
fn content_start_y(area: Rect) -> u16 {
    let count = visible_items(area).min(internal_tools().len());
    let content_height = count + 2; // title + blank line + items
    area.y + (area.height.saturating_sub(content_height as u16)) / 2
}

fn max_row_width() -> usize {
    internal_tools()
        .iter()
        .map(|(name, desc)| 2 + name.len() + 4 + desc.len()) // "✔ name - desc"
        .max()
        .unwrap_or(0)
}

const fn visible_items(area: Rect) -> usize {
    (area.height.saturating_sub(3)) as usize
}
