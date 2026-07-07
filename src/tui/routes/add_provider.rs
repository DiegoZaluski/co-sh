use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_sdk::connector::known_providers_with_env;
use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;

/// All known providers with their API key env var names
fn all_providers() -> Vec<(&'static str, &'static str)> {
    let mut providers: Vec<(&'static str, &'static str)> = known_providers_with_env().collect();
    // Sort alphabetically for consistent display
    providers.sort_by_key(|(name, _)| *name);
    providers
}

pub struct AddProviderView {
    pub selected_index: usize,
    pub scroll_offset: usize,
}

impl AddProviderView {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            scroll_offset: 0,
        }
    }

    pub fn select_next(&mut self, visible_count: usize) {
        let total = all_providers().len();
        if total == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1) % total;
        if self.selected_index >= self.scroll_offset + visible_count {
            self.scroll_offset = self
                .selected_index
                .saturating_sub(visible_count.saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self, _visible_count: usize) {
        let total = all_providers().len();
        if total == 0 {
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            total - 1
        } else {
            self.selected_index - 1
        };
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
    }

    pub fn selected_provider(&self) -> Option<(&'static str, &'static str)> {
        let providers = all_providers();
        providers.get(self.selected_index).copied()
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let providers = all_providers();
        if providers.is_empty() {
            return None;
        }
        let my = mouse.y;
        let mx = mouse.x;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }
        let list_start_y = content_start_y(area) + 2;
        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let visible_count = visible_items(area).min(providers.len());
        let hit = mx >= row_x && mx < row_x + max_row_w as u16;
        if !hit {
            return None;
        }
        for i in 0..visible_count {
            let idx = self.scroll_offset + i;
            if idx >= providers.len() {
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
        let providers = all_providers();
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let title = "ADD Provider";
        let count = visible_items(area).min(providers.len());
        let start_y = content_start_y(area);

        let max_w = max_row_width();
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        // Title
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

        // Provider list
        let list_start_y = start_y + 2;
        if max_w == 0 {
            return;
        }

        // Symbol column + indent
        let sym_x = row_x;
        let name_x = row_x + 2;

        if providers.is_empty() {
            let empty_msg = "No providers available";
            let msg_x = area.x + (area.width.saturating_sub(empty_msg.len() as u16)) / 2;
            for (i, ch) in empty_msg.chars().enumerate() {
                let cx = msg_x + i as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, list_start_y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(muted));
                }
            }
            return;
        }

        for i in 0..count {
            let idx = self.scroll_offset + i;
            if idx >= providers.len() {
                break;
            }
            let (name, env_var) = providers[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_selected = idx == self.selected_index;
            let row_color = if is_selected { primary } else { fg };

            // Check if env var is already set
            let is_configured = std::env::var(env_var).is_ok();
            let symbol = if is_configured { "✔" } else { " " };
            let sym_color = if is_configured {
                Color::Green
            } else {
                row_color
            };

            // Symbol (✔ if configured, space otherwise)
            let sym_ch = symbol.chars().next().unwrap();
            if let Some(cell) = buf.cell_mut((sym_x, y)) {
                cell.set_char(sym_ch);
                cell.set_style(Style::default().fg(sym_color));
            }

            // Provider name + env var name
            let rest = format!("{name}  ({env_var})");
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
    let providers = all_providers();
    let count = visible_items(area).min(providers.len());
    let content_height = count + 2; // title + blank line + items
    area.y + (area.height.saturating_sub(content_height as u16)) / 2
}

fn max_row_width() -> usize {
    all_providers()
        .iter()
        .map(|(name, env_var)| 2 + name.len() + 4 + env_var.len()) // "✔ name  (env_var)"
        .max()
        .unwrap_or(0)
        .max("ADD Provider".len())
}

fn visible_items(area: Rect) -> usize {
    (area.height.saturating_sub(3)) as usize
}
