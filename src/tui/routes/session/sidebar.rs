use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::state::AppState;
use crate::theme::Theme;

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
    pub width: u16,
}

impl SidebarView {
    pub const fn new() -> Self {
        Self {
            open: false,
            width: 24,
        }
    }

    /// Handle a mouse click on the sidebar. Returns an action to perform.
    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect, state: &AppState) -> SidebarAction {
        if !self.open {
            return SidebarAction::None;
        }
        let my = mouse.y;
        let mx = mouse.x;

        if mx < area.x || mx >= area.right() {
            return SidebarAction::None;
        }

        for (i, session) in state.sessions.iter().enumerate() {
            let item_y = area.y + 2 + i as u16;
            if my != item_y {
                continue;
            }

            // Calculate where 🗑 would be: right after the session title
            let is_active = Some(session.id.as_str()) == state.current_session_id.as_deref();
            let prefix = if is_active { "\u{25b8} " } else { "  " };
            let label_len = prefix.chars().count() + session.title.chars().count();
            let max_label_w = area.width.saturating_sub(3) as usize;
            let visible = label_len.min(max_label_w);
            let trash_x = area.x + 2 + visible as u16;

            // Click on 🗑 or the cleared cell after it
            if mx >= trash_x && mx < area.right() {
                return SidebarAction::RequestDelete(session.id.clone());
            }

            // Otherwise, switch to this session
            return SidebarAction::SwitchTo(session.id.clone());
        }

        SidebarAction::None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
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

        let item_style = Style::default().fg(rgba_color(theme.text));
        let active_style = Style::default().fg(rgba_color(theme.primary));
        let delete_style = Style::default().fg(rgba_color(theme.text_muted));

        for (i, session) in state.sessions.iter().enumerate() {
            let y = area.y + 2 + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_active = Some(session.id.as_str()) == state.current_session_id.as_deref();
            let style = if is_active { active_style } else { item_style };

            let prefix = if is_active { "\u{25b8} " } else { "  " };
            let label = format!("{}{}", prefix, session.title);
            // Leave room for 🗑 after the text
            let max_label_w = area.width.saturating_sub(3);
            draw_text_line(buf, &label, area.x + 1, y, max_label_w, style);

            // 🗑 immediately after the visible text
            let label_visible = label.chars().count().min(max_label_w as usize) as u16;
            let trash_x = area.x + 2 + label_visible;
            if trash_x + 1 < area.right()
                && let Some(cell) = buf.cell_mut((trash_x, y))
            {
                cell.set_char('\u{1F5D1}');
                cell.set_style(delete_style);
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
