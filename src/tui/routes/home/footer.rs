use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;

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

pub struct HomeFooterView;

impl HomeFooterView {
    pub fn render(buf: &mut Buffer, area: Rect, theme: &Theme) {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        let muted = Style::default().fg(rgba_color(theme.text_muted));
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
