use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
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

pub struct SidebarView {
    pub open: bool,
    pub width: u16,
}

impl SidebarView {
    pub fn new() -> Self {
        SidebarView {
            open: false,
            width: 24,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        if !self.open {
            return;
        }

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_panel.into()));
        bg_box.render_self(buf, area);

        let header_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, " Sessions", area.x + 1, area.y, area.width.saturating_sub(2), header_style);

        let separator_style = Style::default().fg(rgba_color(theme.border));
        if let Some(cell) = buf.cell_mut((area.x + 1, area.y + 1)) {
            cell.set_char('\u{2500}');
            cell.set_style(separator_style);
        }

        let item_style = Style::default().fg(rgba_color(theme.text));
        let active_style = Style::default().fg(rgba_color(theme.primary));

        for (i, session) in state.sessions.iter().enumerate() {
            let y = area.y + 2 + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_active = Some(session.id.as_str()) == state.current_session_id.as_deref();
            let style = if is_active { active_style } else { item_style };

            let prefix = if is_active { "\u{25b8} " } else { "  " };
            let label = format!("{}{}", prefix, session.title);
            draw_text_line(buf, &label, area.x + 1, y, area.width.saturating_sub(2), style);
        }
    }
}
