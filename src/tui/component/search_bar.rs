use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::Theme;

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
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

    pub fn clear(&mut self) {
        self.filter.clear();
        self.cursor.note_activity();
    }

    pub fn is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.filter
    }

    fn cursor_visible(&self, now: SystemTime) -> bool {
        matches!(self.cursor.current_state(now), CursorState::On)
    }

    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16, width: u16, theme: &Theme) {
        let bg_element = rgba_color(theme.background_element);
        let now = SystemTime::now();

        for cx in x..x + width {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(bg_element));
            }
        }

        let cursor_vis = self.cursor_visible(now);

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
                    .bg(bg_element),
            );

            let cursor_x = x + search_label.len() as u16;
            if cursor_x < x + width
                && let Some(cell) = buf.cell_mut((cursor_x, y))
            {
                if cursor_vis {
                    cell.set_char('\u{2588}');
                    cell.set_style(
                        Style::default()
                            .fg(rgba_color(theme.primary))
                            .bg(bg_element),
                    );
                } else {
                    cell.set_char('\u{2588}');
                    cell.set_style(
                        Style::default()
                            .fg(rgba_color(theme.text_muted))
                            .bg(bg_element),
                    );
                }
            }
        } else {
            draw_text_line(
                buf,
                &self.filter,
                x,
                y,
                width,
                Style::default().fg(rgba_color(theme.text)).bg(bg_element),
            );

            let cursor_x = x + self.filter.len() as u16;
            if cursor_x < x + width
                && let Some(cell) = buf.cell_mut((cursor_x, y))
            {
                if cursor_vis {
                    cell.set_char('\u{2588}');
                    cell.set_style(
                        Style::default()
                            .fg(rgba_color(theme.primary))
                            .bg(bg_element),
                    );
                } else {
                    cell.set_char('\u{2588}');
                    cell.set_style(
                        Style::default()
                            .fg(rgba_color(theme.text_muted))
                            .bg(bg_element),
                    );
                }
            }
        }
    }
}

impl Default for SearchBar {
    fn default() -> Self {
        Self::new()
    }
}
