use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_sdk::connector::{has_api_key, known_providers_with_env};
use cosh_tui::core::types::MouseEvent;

use crate::component::search_bar::SearchBar;
use crate::theme::Theme;
use crate::util::list_selection::ListSelection;

fn all_providers() -> Vec<(&'static str, &'static str)> {
    let mut providers: Vec<(&'static str, &'static str)> = known_providers_with_env().collect();
    providers.sort_by_key(|(name, _)| *name);
    providers
}

pub struct AddProviderView {
    pub selection: ListSelection,
    pub search_bar: SearchBar,
}

impl AddProviderView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            search_bar: SearchBar::new(),
        }
    }

    fn filtered_providers(&self) -> Vec<(&'static str, &'static str)> {
        let providers = all_providers();
        if self.search_bar.is_empty() {
            return providers;
        }
        let lower = self.search_bar.as_str().to_lowercase();
        providers
            .into_iter()
            .filter(|(name, env)| {
                name.to_lowercase().contains(&lower) || env.to_lowercase().contains(&lower)
            })
            .collect()
    }

    fn clamp_selection(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.clamp(self.filtered_providers().len());
    }

    pub fn push_filter_char(&mut self, ch: char, visible_count: usize) {
        self.search_bar.push_char(ch);
        self.clamp_selection(visible_count);
    }

    pub fn pop_filter_char(&mut self, visible_count: usize) {
        self.search_bar.pop_char();
        self.clamp_selection(visible_count);
    }

    pub fn select_next(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_next(self.filtered_providers().len());
    }

    pub fn select_prev(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_prev(self.filtered_providers().len());
    }

    pub fn selected_provider(&self) -> Option<(&'static str, &'static str)> {
        let providers = self.filtered_providers();
        providers.get(self.selection.selected_index).copied()
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let providers = self.filtered_providers();
        if providers.is_empty() {
            return None;
        }
        let my = mouse.y;
        let mx = mouse.x;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }

        let remaining_h = area.height.saturating_sub(1);
        let count = max_visible_items(remaining_h).min(providers.len());
        let block_h = count + 2;
        let block_y = area.y + 1 + (remaining_h.saturating_sub(block_h as u16)) / 2;
        let list_start_y = block_y + 2;

        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let hit = mx >= row_x && mx < row_x + max_row_w as u16;
        if !hit {
            return None;
        }
        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
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
        let providers = self.filtered_providers();
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let max_w = max_row_width();
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        // Title pinned to top
        let title_y = area.y;
        for (i, ch) in "ADD Provider".chars().enumerate() {
            let cx = row_x + i as u16;
            if cx >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, title_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(muted));
            }
        }

        // Search + gap + items: vertically centered below the title
        let remaining_h = area.height.saturating_sub(1);
        let count = max_visible_items(remaining_h).min(providers.len());
        let block_h = count + 2;
        let block_y = area.y + 1 + (remaining_h.saturating_sub(block_h as u16)) / 2;

        let search_w = max_w.min(area.width.saturating_sub(row_x) as usize);
        self.search_bar
            .render(buf, row_x, block_y, search_w as u16, theme);

        let list_start_y = block_y + 2;
        if max_w == 0 {
            return;
        }

        let sym_x = row_x;
        let name_x = row_x + 2;

        if providers.is_empty() {
            let empty_msg = if self.search_bar.is_empty() {
                "No providers available"
            } else {
                "No matching providers"
            };
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
            let idx = self.selection.scroll_offset + i;
            if idx >= providers.len() {
                break;
            }
            let (name, env_var) = providers[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_selected = idx == self.selection.selected_index;
            let row_color = if is_selected { primary } else { fg };

            let is_configured = has_api_key(name);
            let symbol = if is_configured { "✔" } else { " " };
            let sym_color = if is_configured {
                Color::Green
            } else {
                row_color
            };

            let sym_ch = symbol.chars().next().unwrap();
            if let Some(cell) = buf.cell_mut((sym_x, y)) {
                cell.set_char(sym_ch);
                cell.set_style(Style::default().fg(sym_color));
            }

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

fn max_row_width() -> usize {
    all_providers()
        .iter()
        .map(|(name, env_var)| 2 + name.len() + 4 + env_var.len())
        .max()
        .unwrap_or(0)
        .max("ADD Provider".len())
}

const fn max_visible_items(remaining_h: u16) -> usize {
    (remaining_h.saturating_sub(3)) as usize
}
