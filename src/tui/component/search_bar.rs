use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

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
    pub last_filter_at: SystemTime,
    pub blink_start: SystemTime,
}

impl SearchBar {
    pub fn new() -> Self {
        Self {
            filter: String::new(),
            last_filter_at: SystemTime::now(),
            blink_start: SystemTime::now(),
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.last_filter_at = SystemTime::now();
        self.blink_start = SystemTime::now();
    }

    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.last_filter_at = SystemTime::now();
        self.blink_start = SystemTime::now();
    }

    pub fn clear(&mut self) {
        self.filter.clear();
        self.last_filter_at = SystemTime::now();
        self.blink_start = SystemTime::now();
    }

    pub fn is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.filter
    }

    fn cursor_visible(&self, now: SystemTime) -> bool {
        let idle_ms = now
            .duration_since(self.last_filter_at)
            .map_or(0, |d| d.as_millis());
        if idle_ms < 500 {
            true
        } else {
            let elapsed_ms = now
                .duration_since(self.blink_start)
                .map_or(0, |d| d.as_millis() % 1000);
            elapsed_ms < 500
        }
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

        if !self.filter.is_empty() {
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
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(bg_element));
                }
            }
        } else {
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
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(bg_element));
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
