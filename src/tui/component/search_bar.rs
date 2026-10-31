use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::component::cursor::{Cursor, paint_cursor_cell};
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;

pub struct SearchBar {
    pub filter: String,
    pub cursor: Cursor,
}

impl SearchBar {
    pub fn new() -> Self {
        Self {
            filter: String::new(),
            cursor: Cursor::new(),
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.cursor.note_activity();
    }

    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.cursor.note_activity();
    }

    pub const fn is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.filter
    }

    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16, width: u16, theme: &Theme) {
        let bg_color = rgba_color(theme.background);
        let now = SystemTime::now();

        for cx in x..x + width {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(bg_color));
            }
        }

        if self.filter.is_empty() {
            let search_label = "Search";
            draw_text_line(
                buf,
                search_label,
                x,
                y,
                width,
                Style::default()
                    .fg(rgba_color(theme.text_muted))
                    .bg(bg_color),
            );

            // Cursor positioned over the first character of the placeholder.
            // Transparent style: the glyph under the caret stays visible.
            let cursor_state = self.cursor.current_state(now);
            if let Some(cell) = buf.cell_mut((x, y)) {
                paint_cursor_cell(
                    cell,
                    cursor_state,
                    bg_color,
                    rgba_color(theme.text),
                    rgba_color(theme.text_muted),
                );
            }
        } else {
            draw_text_line(
                buf,
                &self.filter,
                x,
                y,
                width,
                Style::default().fg(rgba_color(theme.text)).bg(bg_color),
            );

            let cursor_x = x + self.filter.len() as u16;
            if cursor_x < x + width
                && let Some(cell) = buf.cell_mut((cursor_x, y))
            {
                // Transparent style: the glyph under the caret stays visible.
                paint_cursor_cell(
                    cell,
                    self.cursor.current_state(now),
                    bg_color,
                    rgba_color(theme.text),
                    rgba_color(theme.text_muted),
                );
            }
        }
    }
}

impl Default for SearchBar {
    fn default() -> Self {
        Self::new()
    }
}
