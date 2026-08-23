//! Settings router — a simple selectable list of configuration categories.
//!
//! Unlike the other flat-list routers (`tools`, `add_provider`), each option
//! carries an explanatory description rendered directly above it, telling
//! the user what the setting controls.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;
use crate::util::list_selection::ListSelection;
use crate::util::setup::Setup;

/// Rows consumed by one entry: description + option + blank spacer.
const ROWS_PER_ITEM: u16 = 3;

/// One entry of the Settings list. `id` keys the activation behaviour;
/// `label` is the option name; `description` explains what the setting does.
struct SettingsItem {
    id: &'static str,
    label: &'static str,
    description: &'static str,
}

fn settings_items() -> &'static [SettingsItem] {
    &[SettingsItem {
        id: "hooks",
        label: "Hooks",
        description: "Run custom shell commands before every tool call",
    }]
}

/// Whether the given setting is currently on, read from persisted state.
fn is_enabled(item: &SettingsItem, setup: &Setup) -> bool {
    match item.id {
        "hooks" => setup.hooks.enabled,
        _ => true,
    }
}

pub struct SettingsView {
    pub selection: ListSelection,
}

impl SettingsView {
    pub const fn new() -> Self {
        Self {
            selection: ListSelection::new(),
        }
    }

    pub fn select_next(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_next(settings_items().len());
    }

    pub fn select_prev(&mut self, visible_count: usize) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_prev(settings_items().len());
    }

    /// Activate the selected setting (toggle its persisted state).
    /// Returns whether any state changed.
    pub fn activate_current(&self, setup: &mut Setup) -> bool {
        let item = &settings_items()[self.selection.selected_index];
        match item.id {
            "hooks" => {
                setup.hooks.enabled = !setup.hooks.enabled;
                true
            }
            _ => false,
        }
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let mx = mouse.x;
        let my = mouse.y;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }
        let list_start_y = content_start_y(area) + 2;
        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let visible_count = visible_items(area);
        // Only the option row is clickable, not its description or spacer.
        let hit_x = mx >= row_x && mx < row_x + max_row_w as u16;
        if !hit_x {
            return None;
        }
        for i in 0..visible_count {
            let idx = self.selection.scroll_offset + i;
            if idx >= settings_items().len() {
                break;
            }
            let option_y = list_start_y + i as u16 * ROWS_PER_ITEM + 1;
            if my == option_y {
                return Some(idx);
            }
        }
        None
    }

    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        self.find_row_for_mouse(mouse, area)
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, setup: &Setup) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let items = settings_items();
        let title = "Settings";
        let count = visible_items(area).min(items.len());
        let start_y = content_start_y(area);

        let max_w = max_row_width();
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        // Title (muted), like the other routers.
        for (i, ch) in title.chars().enumerate() {
            let cx = row_x + i as u16;
            if cx >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, start_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(muted));
            }
        }

        // Clamp selection and scroll before rendering.
        self.selection.set_visible_count(count);
        self.selection.clamp(items.len());

        let list_start_y = start_y + 2;
        if max_w == 0 {
            return;
        }

        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
            if idx >= items.len() {
                break;
            }
            let item = &items[idx];
            let desc_y = list_start_y + i as u16 * ROWS_PER_ITEM;
            let opt_y = desc_y + 1;
            if opt_y >= area.bottom() {
                break;
            }

            // Description above the option (muted).
            for (j, ch) in item.description.chars().enumerate() {
                let cx = row_x + j as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, desc_y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(muted));
                }
            }

            let enabled = is_enabled(item, setup);
            let symbol = if enabled { "✔" } else { "✗" };
            let sym_color = if enabled { Color::Green } else { Color::Red };
            let is_selected = idx == self.selection.selected_index;
            let label_color = if is_selected { primary } else { fg };

            // Symbol column (zero-width prefix so column 0 = symbol).
            let sym_ch = symbol.chars().next().unwrap();
            if let Some(cell) = buf.cell_mut((row_x, opt_y)) {
                cell.set_char(sym_ch);
                cell.set_style(Style::default().fg(sym_color));
            }

            for (j, ch) in item.label.chars().enumerate() {
                let cx = row_x + 2 + j as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, opt_y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(label_color));
                }
            }
        }
    }
}

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Compute the Y position where title starts (vertically centered block).
fn content_start_y(area: Rect) -> u16 {
    let count = visible_items(area).min(settings_items().len());
    // title + blank line, plus one option/description pair (+spacer) per item,
    // minus the trailing spacer of the last item.
    let mut content_height = 2u16;
    if count > 0 {
        content_height += count as u16 * ROWS_PER_ITEM - 1;
    }
    area.y + (area.height.saturating_sub(content_height)) / 2
}

fn max_row_width() -> usize {
    settings_items()
        .iter()
        .map(|item| item.description.len().max(2 + item.label.len())) // "✔ Hooks"
        .max()
        .unwrap_or(0)
}

const fn visible_items(area: Rect) -> usize {
    area.height.saturating_sub(3) as usize / ROWS_PER_ITEM as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

    fn test_theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    fn line_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.x..area.right())
            .map(|x| {
                buf.cell((x, y))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Row index containing `needle`, or None.
    fn row_of(buf: &Buffer, area: Rect, needle: &str) -> Option<u16> {
        (area.y..area.bottom()).find(|&y| line_text(buf, area, y).contains(needle))
    }

    #[test]
    fn renders_title_and_description_above_option() {
        let theme = test_theme();
        let setup = Setup::default();
        let mut view = SettingsView::new();
        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let title_y = row_of(&buf, area, "Settings").expect("title rendered");
        let desc_y = row_of(&buf, area, "Run custom shell commands before every tool call")
            .expect("description rendered");
        let opt_text = line_text(&buf, area, desc_y + 1);
        assert!(opt_text.contains("Hooks"), "option below description, got {opt_text:?}");
        assert!(desc_y > title_y, "description comes after the title");
        assert!(
            opt_text.contains('✔'),
            "hooks default to enabled, got {opt_text:?}"
        );
    }

    #[test]
    fn activate_current_toggles_hooks_enabled() {
        let mut setup = Setup::default();
        assert!(setup.hooks.enabled);
        let mut view = SettingsView::new();
        view.selection.clamp(settings_items().len());

        assert!(view.activate_current(&mut setup));
        assert!(!setup.hooks.enabled);
        assert!(view.activate_current(&mut setup));
        assert!(setup.hooks.enabled);
    }

    #[test]
    fn render_reflects_disabled_state() {
        let theme = test_theme();
        let mut setup = Setup::default();
        setup.hooks.enabled = false;
        let mut view = SettingsView::new();
        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let opt_y = row_of(&buf, area, "Hooks").expect("option rendered");
        assert!(line_text(&buf, area, opt_y).contains('✗'));
    }

    #[test]
    fn mouse_hits_option_row_only() {
        let theme = test_theme();
        let setup = Setup::default();
        let mut view = SettingsView::new();
        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let opt_y = row_of(&buf, area, "Hooks").expect("option rendered");
        // Click on the label character: block is centered like in render.
        let row_x = area.x + (area.width.saturating_sub(max_row_width() as u16)) / 2;
        let click = MouseEvent::new(
            MouseEventType::Move,
            MouseButton::Left,
            row_x + 2,
            opt_y,
            MouseModifiers::none(),
        );
        assert_eq!(view.handle_mouse(&click, area), Some(0));
        // Description row and spacer are not clickable.
        let miss = |y| {
            MouseEvent::new(
                MouseEventType::Move,
                MouseButton::Left,
                row_x,
                y,
                MouseModifiers::none(),
            )
        };
        assert_eq!(view.handle_mouse(&miss(opt_y - 1), area), None);
        assert_eq!(view.handle_mouse(&miss(opt_y + 1), area), None);
    }
}
