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

pub struct HomeView;

impl HomeView {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState, theme: &Theme) {
        let cx = area.x + area.width / 2;

        let logo = "cosh";
        let logo_style = Style::default().fg(rgba_color(theme.primary));
        let logo_x = cx.saturating_sub(logo.len() as u16 / 2);
        draw_text_line(buf, logo, logo_x, area.y + 2, area.width, logo_style);

        let tagline = "Terminal AI Agent";
        let tagline_style = Style::default().fg(rgba_color(theme.text_muted));
        let tagline_x = cx.saturating_sub(tagline.len() as u16 / 2);
        draw_text_line(buf, tagline, tagline_x, area.y + 4, area.width, tagline_style);

        if state.sessions.is_empty() {
            let new_session = "Start a new session";
            let new_style = Style::default().fg(rgba_color(theme.text));
            let new_x = cx.saturating_sub(new_session.len() as u16 / 2);
            draw_text_line(buf, new_session, new_x, area.y + 8, area.width, new_style);
        } else {
            let recent = "Recent Sessions";
            let recent_style = Style::default().fg(rgba_color(theme.text));
            let recent_x = cx.saturating_sub(recent.len() as u16 / 2);
            draw_text_line(buf, recent, recent_x, area.y + 7, area.width, recent_style);

            for (i, session) in state.sessions.iter().enumerate() {
                let sy = area.y + 9 + i as u16;
                if sy >= area.bottom() {
                    break;
                }

                let is_active = Some(session.id.as_str()) == state.current_session_id.as_deref();
                let marker = if is_active { "\u{25b8}" } else { " " };
                let entry = format!(" {}  {} ({} msgs)", marker, session.title, session.messages.len());
                let entry_style = if is_active {
                    Style::default().fg(rgba_color(theme.primary))
                } else {
                    Style::default().fg(rgba_color(theme.text))
                };
                let entry_x = cx.saturating_sub(entry.len() as u16 / 2);
                draw_text_line(buf, &entry, entry_x, sy, area.width, entry_style);
            }
        }

        let key_hints = "n: new session  q: quit  ?: help";
        let hint_style = Style::default().fg(rgba_color(theme.text_muted));
        let hint_x = cx.saturating_sub(key_hints.len() as u16 / 2);
        draw_text_line(buf, key_hints, hint_x, area.bottom().saturating_sub(2), area.width, hint_style);
    }
}
