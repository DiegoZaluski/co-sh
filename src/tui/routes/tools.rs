// TODO: Stumb para adicionar lógica de persistência das opções de ferramentas.
// Atualmente as opções reiniciam ao fechar o programa.
use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;

const INTERNAL_TOOLS: &[(&str, &str)] = &[
    ("bash_run", "Execute shell commands"),
    ("fs_read", "Read file contents"),
    ("fs_write", "Write file contents"),
    ("fs_edit", "Edit file contents"),
    ("fs_rollback", "Rollback file changes"),
    ("find_glob", "Find files by glob pattern"),
    ("find_grep", "Search file contents"),
    ("web_fetch", "Fetch web page content"),
    ("web_search", "Search the web"),
    ("vision_terminal", "Capture terminal output"),
    ("plan_todo_write", "Write todo items"),
    ("plan_todo_edit", "Edit todo items"),
    ("plan_todo_cross_off", "Cross off todo items"),
    ("plan_todo_read", "Read todo items"),
    ("plan_load_from_md", "Load plan from markdown"),
    ("ask_questions", "Ask the user questions"),
    ("skills_list", "List available skills"),
    ("skills_read", "Read a skill"),
    ("skills_read_asset", "Read a skill asset"),
    ("skills_match_skills", "Match skills to task"),
];

pub struct InternalToolsView {
    pub selected_index: usize,
    pub disabled: HashSet<String>,
    pub changed: bool,
    scroll_offset: usize,
}

impl InternalToolsView {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            disabled: HashSet::new(),
            changed: false,
            scroll_offset: 0,
        }
    }

    pub fn select_next(&mut self, visible_count: usize) {
        let total = INTERNAL_TOOLS.len();
        self.selected_index = (self.selected_index + 1) % total;
        if self.selected_index >= self.scroll_offset + visible_count {
            self.scroll_offset = self
                .selected_index
                .saturating_sub(visible_count.saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self, _visible_count: usize) {
        let total = INTERNAL_TOOLS.len();
        self.selected_index = if self.selected_index == 0 {
            total - 1
        } else {
            self.selected_index - 1
        };
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
    }

    pub fn toggle_current(&mut self) {
        let (name, _) = INTERNAL_TOOLS[self.selected_index];
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
            let idx = self.scroll_offset + i;
            if idx >= INTERNAL_TOOLS.len() {
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

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        // title
        let title = "Internal Tools";
        let count = visible_items(area).min(INTERNAL_TOOLS.len());
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

        // tools list aligned
        let list_start_y = start_y + 2;
        if max_w == 0 {
            return;
        }

        // "  " column for the symbol (ZERO WIDTH prefix so column 0 = symbol)
        let sym_x = row_x;
        let name_x = row_x + 2; // symbol + 1 space

        for i in 0..count {
            let idx = self.scroll_offset + i;
            if idx >= INTERNAL_TOOLS.len() {
                break;
            }
            let (name, desc) = INTERNAL_TOOLS[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let enabled = !self.disabled.contains(name);
            let symbol = if enabled { "✔" } else { "✗" };
            let sym_color = if enabled { Color::Green } else { Color::Red };
            let is_selected = idx == self.selected_index;
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
    let count = visible_items(area).min(INTERNAL_TOOLS.len());
    let content_height = count + 2; // title + blank line + items
    area.y + (area.height.saturating_sub(content_height as u16)) / 2
}

fn max_row_width() -> usize {
    INTERNAL_TOOLS
        .iter()
        .map(|(name, desc)| 2 + name.len() + 4 + desc.len()) // "✔ name - desc"
        .max()
        .unwrap_or(0)
}

fn visible_items(area: Rect) -> usize {
    (area.height.saturating_sub(3)) as usize
}
