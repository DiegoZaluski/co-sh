use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;

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
