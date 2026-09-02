use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::util::list_selection::ListSelection;
use ratatui::crossterm::event::KeyCode;

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters (session titles derive from user message
        // text, which can contain `\n`/`\t`): writing them into cells makes
        // ratatui's buffer diff panic ("control character passed to
        // cell_width without filtering").
        if ch.is_control() {
            continue;
        }
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Action returned by the sidebar after a mouse click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarAction {
    /// Switch to the given session.
    SwitchTo(String),
    /// Request deletion of the given session.
    RequestDelete(String),
    /// No action.
    None,
}

pub struct SidebarView {
    pub open: bool,
    /// Shared list-selection state: selected_index + scroll_offset.
    pub selection: ListSelection,
}

impl SidebarView {
    pub const fn new() -> Self {
        Self {
            open: false,
            selection: ListSelection::new(),
        }
    }

    /// Delegate to `selection.select_prev`.
    pub fn select_prev(&mut self, total: usize) {
        self.selection.select_prev(total);
    }

    /// Delegate to `selection.select_next`.
    pub fn select_next(&mut self, total: usize) {
        self.selection.select_next(total);
    }

    /// Delegate to `selection.select_first`.
    pub fn select_first(&mut self, total: usize) {
        self.selection.select_first(total);
    }

    /// Delegate to `selection.select_last`.
    pub fn select_last(&mut self, total: usize) {
        self.selection.select_last(total);
    }

    /// Handle a mouse click on the sidebar. Returns an action to perform.
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        state: &AppState,
    ) -> SidebarAction {
        if !self.open {
            return SidebarAction::None;
        }
        let my = mouse.y;
        let mx = mouse.x;

        if mx < area.x || mx >= area.right() {
            return SidebarAction::None;
        }

        // Click must be within the list area (below header + separator)
        if my < area.y + 2 || my >= area.bottom() {
            return SidebarAction::None;
        }

        // Compute which item was clicked, accounting for scroll offset
        let list_offset = i32::from(my) - i32::from(area.y + 2);
        let clicked_idx = self.selection.scroll_offset + list_offset as usize;
        let Some(summary) = state.session_summaries.get(clicked_idx) else {
            return SidebarAction::None;
        };

        // Update selection to clicked item
        let total = state.session_summaries.len();
        if clicked_idx < total && state.status == crate::types::SessionStatus::Idle {
            self.selection.selected_index = clicked_idx;
        }

        // 🗑 hit-test: same calculation as render (left_pad + prefix + text + gap)
        let label_len = summary.title.chars().count();
        let max_text_w = area.width.saturating_sub(10) as usize;
        let visible = label_len.min(max_text_w);
        let trash_x = area.x + 2 + 2 + visible as u16 + 1;

        // Click on 🗑 or the cleared cell after it
        if mx >= trash_x && mx < area.right() {
            return SidebarAction::RequestDelete(summary.session_id.clone());
        }

        // Otherwise, switch to this session
        SidebarAction::SwitchTo(summary.session_id.clone())
    }

    /// Handle a key press on the sidebar. Returns an action to perform.
    pub fn handle_key(&mut self, key: KeyCode, state: &AppState) -> SidebarAction {
        if !self.open {
            return SidebarAction::None;
        }
        match key {
            KeyCode::Enter => {
                let idx = self.selection.selected_index;
                let Some(summary) = state.session_summaries.get(idx) else {
                    return SidebarAction::None;
                };
                SidebarAction::SwitchTo(summary.session_id.clone())
            }
            _ => SidebarAction::None,
        }
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        if !self.open {
            return;
        }

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_panel.into()));
        bg_box.render_self(buf, area);

        let header_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(
            buf,
            " Sessions",
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            header_style,
        );

        let separator_style = Style::default().fg(rgba_color(theme.border));
        if let Some(cell) = buf.cell_mut((area.x + 1, area.y + 1)) {
            cell.set_char('\u{2500}');
            cell.set_style(separator_style);
        }

        // ── Clamp and scroll ──
        let content_start_y = area.y + 2;
        let visible_count = area.bottom().saturating_sub(content_start_y) as usize;
        let total_items = state.session_summaries.len();

        self.selection.set_visible_count(visible_count);
        self.selection.clamp(total_items);

        // ── Styles ──
        let primary_color = rgba_color(theme.primary);
        let text_color = rgba_color(theme.text);
        let mute_fg = rgba_color(theme.text_muted);

        let selected_fg = Style::default().fg(primary_color);
        let normal_fg = Style::default().fg(text_color);

        // Layout: left_pad(2) + prefix(2) + text + gap(1) + 🗑(2) + clear(1) + right_pad(2)
        //   text max = width - 10
        let left_pad: u16 = 2;
        let prefix_w: u16 = 2;
        let text_x = area.x + left_pad + prefix_w;
        let max_text_w = area
            .width
            .saturating_sub(left_pad + prefix_w + 1 + 2 + 1 + 2);

        for (i, summary) in state
            .session_summaries
            .iter()
            .enumerate()
            .skip(self.selection.scroll_offset)
        {
            let local_idx = i - self.selection.scroll_offset;
            let y = content_start_y + local_idx as u16;
            if y >= area.bottom() {
                break;
            }

            let is_selected = i == self.selection.selected_index;
            let style = if is_selected { selected_fg } else { normal_fg };
            let px = area.x + left_pad;

            // Prefix indicator — drawn manually to avoid draw_text_line breaking wide emoji
            if is_selected {
                // 🞴 emoji at px (takes 2 cols — don't touch px+1)
                if let Some(cell) = buf.cell_mut((px, y)) {
                    cell.set_char('\u{1F7B4}');
                    cell.set_style(style);
                }
            } else {
                // 2 spaces for alignment
                if let Some(cell) = buf.cell_mut((px, y)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
                if let Some(cell) = buf.cell_mut((px + 1, y)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }

            // Text
            let label = &summary.title;
            draw_text_line(buf, label, text_x, y, max_text_w, style);

            let label_visible = label.chars().count().min(max_text_w as usize) as u16;

            // 🗑 after 1 gap (NO bg)
            let trash_x = text_x + label_visible + 1;
            if trash_x + 1 < area.right()
                && let Some(cell) = buf.cell_mut((trash_x, y))
            {
                cell.set_char('\u{1F5D1}');
                cell.set_style(Style::default().fg(mute_fg));
            }
            // Clear the cell after the wide emoji
            if trash_x + 1 < area.right()
                && let Some(cell) = buf.cell_mut((trash_x + 1, y))
            {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }
    }
}
