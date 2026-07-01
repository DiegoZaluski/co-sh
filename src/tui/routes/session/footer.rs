use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;

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

pub struct FooterView;

impl FooterView {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        if let Some(session) = state.current_session() {
            let left = format!(
                " {}  {} msgs  q:quit  \u{2191}\u{2195}:scroll",
                session.title,
                session.messages.len()
            );
            let left_style = Style::default().fg(rgba_color(theme.text_muted));
            draw_text_line(buf, &left, area.x + 1, area.y, area.width.saturating_sub(2), left_style);

            let right = "b:sidebar  ?:help";
            let right_style = Style::default().fg(rgba_color(theme.text_muted));
            let right_x = area.right().saturating_sub(right.len() as u16 + 1);
            draw_text_line(buf, right, right_x, area.y, area.width.saturating_sub(1), right_style);
        } else {
            let text = " \u{2302} Home";
            let style = Style::default().fg(rgba_color(theme.text_muted));
            draw_text_line(buf, text, area.x + 1, area.y, area.width.saturating_sub(2), style);
        }
    }
}
