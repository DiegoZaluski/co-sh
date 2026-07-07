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
        Self::render_with_mode(buf, area, state, theme, true);
    }

    pub fn render_with_mode(
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        show_home_label: bool,
    ) {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        let muted = Style::default().fg(rgba_color(theme.text_muted));
        let success = Style::default().fg(rgba_color(theme.success));
        let warning = Style::default().fg(rgba_color(theme.warning));

        if let Some(session) = state.current_session() {
            let dir = &state.working_directory;
            let dir_display = if dir.is_empty() { "~" } else { dir };

            let left = format!(
                " {}  {} msgs \u{2191}\u{2193}:scroll",
                session.title,
                session.messages.len()
            );
            draw_text_line(
                buf,
                &left,
                area.x + 1,
                area.y,
                area.width.saturating_sub(2),
                muted,
            );

            let mut rx = area.right().saturating_sub(2);

            let dir_str = format!(" {dir_display}");
            rx = rx.saturating_sub(dir_str.len() as u16);
            draw_text_line(buf, &dir_str, rx, area.y, dir_str.len() as u16, muted);

            let conn_indicator = if state.connected {
                "\u{25cf}"
            } else {
                "\u{25cb}"
            };
            let conn_str = format!(" {conn_indicator}");
            rx = rx.saturating_sub(conn_str.len() as u16);
            let conn_style = if state.connected { success } else { warning };
            draw_text_line(
                buf,
                &conn_str,
                rx,
                area.y,
                conn_str.len() as u16,
                conn_style,
            );

            if state.permission_count > 0 {
                let s = format!("  perm:{}", state.permission_count);
                rx = rx.saturating_sub(s.len() as u16);
                draw_text_line(buf, &s, rx, area.y, s.len() as u16, muted);
            }

            if state.mcp_count > 0 || state.mcp_errors > 0 {
                let s = if state.mcp_errors > 0 {
                    format!("  mcp:{}/{}", state.mcp_count, state.mcp_errors)
                } else {
                    format!("  mcp:{}", state.mcp_count)
                };
                rx = rx.saturating_sub(s.len() as u16);
                draw_text_line(buf, &s, rx, area.y, s.len() as u16, muted);
            }

            if state.lsp_count > 0 {
                let s = format!("  lsp:{}", state.lsp_count);
                rx = rx.saturating_sub(s.len() as u16);
                draw_text_line(buf, &s, rx, area.y, s.len() as u16, muted);
            }
        } else if show_home_label {
            let text = " \u{2302} Home";
            draw_text_line(
                buf,
                text,
                area.x + 1,
                area.y,
                area.width.saturating_sub(2),
                muted,
            );
        }
    }
}
