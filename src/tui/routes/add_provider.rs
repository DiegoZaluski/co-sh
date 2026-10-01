use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_sdk::connector::{
    get_provider, get_provider_env_var, has_api_key, is_local_provider, known_providers,
};
use cosh_tui::core::types::MouseEvent;

use crate::component::search_bar::SearchBar;
use crate::theme::{Theme, rgba_color};
use crate::util::list_selection::ListSelection;
use cosh::setup::Setup;

/// A provider entry shown in the ADD Provider list. Local providers
/// (ollama, llamacpp, …) are configured with a base URL; cloud providers with
/// an API key.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub name: &'static str,
    pub local: bool,
    /// Cloud: the API key env var name. Local: the default server URL.
    pub hint: String,
}

impl ProviderEntry {
    /// Whether the provider is considered configured.
    /// Cloud → an API key is available (env or keyring). Local → a base URL
    /// was saved in setup.json.
    #[must_use]
    pub fn is_configured(&self, setup: &Setup) -> bool {
        if self.local {
            setup.local_base_url(self.name).is_some()
        } else {
            has_api_key(self.name)
        }
    }
}

fn all_providers() -> Vec<ProviderEntry> {
    let mut providers: Vec<ProviderEntry> = known_providers()
        .map(|name| {
            let local = is_local_provider(name);
            let hint = if local {
                get_provider(name)
                    .map(|cfg| cfg.base_url.to_string())
                    .unwrap_or_default()
            } else {
                get_provider_env_var(name).unwrap_or("").to_string()
            };
            ProviderEntry { name, local, hint }
        })
        .collect();
    providers.sort_by_key(|p| p.name);
    providers
}

pub struct AddProviderView {
    pub selection: ListSelection,
    pub search_bar: SearchBar,
    /// How many rows actually fit in the current viewport. Updated on every
    /// render so that scrolling, clamping and mouse hit-testing all agree on
    /// the same value — otherwise items scroll out of view while free space
    /// below the list is wasted.
    visible_count: usize,
    /// Buffer row of the first visible list item, captured at render time.
    /// The vertical centering depends on the exact area the render used, so
    /// the mouse path must reuse it instead of re-deriving it from its own
    /// (differently sized) area — re-deriving shifted every hit by one row.
    list_start_y: u16,
}

impl AddProviderView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            search_bar: SearchBar::new(),
            visible_count: 0,
            list_start_y: 0,
        }
    }

    fn filtered_providers(&self) -> Vec<ProviderEntry> {
        let providers = all_providers();
        if self.search_bar.is_empty() {
            return providers;
        }
        let lower = self.search_bar.as_str().to_lowercase();
        providers
            .into_iter()
            .filter(|entry| {
                entry.name.to_lowercase().contains(&lower)
                    || entry.hint.to_lowercase().contains(&lower)
            })
            .collect()
    }

    fn clamp_selection(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.clamp(self.filtered_providers().len());
    }

    pub fn push_filter_char(&mut self, ch: char) {
        self.search_bar.push_char(ch);
        self.clamp_selection();
    }

    pub fn pop_filter_char(&mut self) {
        self.search_bar.pop_char();
        self.clamp_selection();
    }

    pub fn select_next(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_next(self.filtered_providers().len());
    }

    pub fn select_prev(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_prev(self.filtered_providers().len());
    }

    pub fn selected_provider(&self) -> Option<ProviderEntry> {
        let providers = self.filtered_providers();
        providers.get(self.selection.selected_index).cloned()
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let providers = self.filtered_providers();
        if providers.is_empty() {
            return None;
        }
        // Use the viewport the render computed, so hit-testing matches
        // what is actually drawn on screen. The vertical geometry must come
        // from the render: the mouse path receives a slightly different
        // area, and re-deriving the centered block from it shifted every
        // row by one.
        let count = self.visible_count.min(providers.len());
        if count == 0 {
            return None;
        }
        let my = mouse.y;
        let mx = mouse.x;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }

        let list_start_y = self.list_start_y;

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

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, setup: &Setup) {
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
        self.visible_count = count;
        let block_h = count + 2;
        let block_y = area.y + 1 + (remaining_h.saturating_sub(block_h as u16)) / 2;
        let search_w = max_w.min(area.width.saturating_sub(row_x) as usize);
        self.search_bar
            .render(buf, row_x, block_y, search_w as u16, theme);

        let list_start_y = block_y + 2;
        self.list_start_y = list_start_y;
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
            let entry = &providers[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_selected = idx == self.selection.selected_index;
            let row_color = if is_selected { primary } else { fg };

            let is_configured = entry.is_configured(setup);
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

            let rest = format!(
                "{}{}  ({})",
                entry.name,
                if entry.local { " (local)" } else { "" },
                entry.hint
            );
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

fn max_row_width() -> usize {
    all_providers()
        .iter()
        .map(|entry| 2 + entry.name.len() + 8 + entry.hint.len())
        .max()
        .unwrap_or(0)
        .max("ADD Provider".len())
}

const fn max_visible_items(remaining_h: u16) -> usize {
    (remaining_h.saturating_sub(3)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            MouseEventType::Down,
            MouseButton::Left,
            x,
            y,
            MouseModifiers::none(),
        )
    }

    /// Regression: `find_row_for_mouse` must reuse the list geometry the
    /// render captured instead of re-deriving it from the mouse path's area,
    /// which is one row taller than the render's and shifted every hit down
    /// by a row.
    #[test]
    fn mouse_hits_match_the_rendered_list_rows() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        let setup = Setup::default();
        let term = Rect::new(0, 0, 100, 30);

        // Render path (src/tui/app/render.rs): `session_area.height` is the
        // terminal height − 3 and the AddProvider branch subtracts 1 more.
        let render_area = Rect::new(0, 1, 100, term.height - 4);
        // Mouse path (src/tui/app/mouse.rs): terminal height − 3, one row
        // taller than the render area.
        let mouse_area = Rect::new(0, 1, 100, term.height - 3);

        let mut view = AddProviderView::new();
        let mut buf = Buffer::empty(term);
        view.render(&mut buf, render_area, &theme, &setup);

        assert!(
            view.visible_count > 0,
            "viewport must show at least one row"
        );
        let max_w = max_row_width();
        let row_x = render_area.x + (render_area.width.saturating_sub(max_w as u16)) / 2;
        let name_x = row_x + 2;

        // Sanity: a provider really is drawn on the first list row.
        let drawn = (0..10u16).any(|dx| {
            buf.cell((name_x + dx, view.list_start_y))
                .is_some_and(|cell| cell.symbol() != " ")
        });
        assert!(
            drawn,
            "no provider drawn at first list row y={}",
            view.list_start_y
        );

        // Every drawn row must map to its own index even though the mouse
        // path hands us a differently sized area.
        for i in 0..view.visible_count.min(5) {
            let y = view.list_start_y + i as u16;
            assert_eq!(
                view.handle_mouse(&click(name_x, y), mouse_area),
                Some(i),
                "click on drawn row y={y} misaligned"
            );
        }

        // Rows just outside the drawn list must not hit anything.
        assert_eq!(
            view.handle_mouse(&click(name_x, view.list_start_y - 1), mouse_area),
            None
        );
        let below = view.list_start_y + view.visible_count as u16;
        assert_eq!(view.handle_mouse(&click(name_x, below), mouse_area), None);
    }
}
