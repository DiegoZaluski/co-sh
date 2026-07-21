//! Input component for the RAG feature (URLs, file paths).
//!
//! Styled like the session prompt but single-line — left vertical border
//! `┃` (`\u{2503}`), `background_element` fill, blinking cursor, paste
//! support.  The border colour comes from `theme.primary` (matching the
//! session prompt's agent colour).

use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::Theme;

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

const PLACEHOLDER: &str = "Enter URL or file path, then press Enter";

pub struct RagInput {
    pub text: String,
    pub cursor_pos: usize,
    pub cursor: Cursor,
    pub is_focused: bool,
}

impl RagInput {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
            cursor: Cursor::new(),
            is_focused: true,
        }
    }

    pub fn focus(&mut self) {
        self.is_focused = true;
    }
    pub fn blur(&mut self) {
        self.is_focused = false;
    }

    pub fn push_char(&mut self, ch: char) {
        self.text.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
        self.cursor.note_activity();
    }

    pub fn pop_char(&mut self) {
        if self.cursor_pos == 0 || self.text.is_empty() {
            return;
        }
        let char_start = self.text.floor_char_boundary(self.cursor_pos - 1);
        self.text.remove(char_start);
        self.cursor_pos = char_start;
        self.cursor.note_activity();
    }

    pub fn delete_forward(&mut self) {
        let len = self.text.len();
        if self.cursor_pos >= len {
            return;
        }
        let next = self.text.floor_char_boundary(self.cursor_pos + 1).min(len);
        self.text.drain(self.cursor_pos..next);
        self.cursor.note_activity();
    }

    /// Paste text into the input.  Normalises line endings and strips
    /// control characters (saves the cursor position first, then
    /// re-establishes it at the end of the inserted text).
    pub fn handle_paste(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let cleaned: String = normalized
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        self.text.insert_str(self.cursor_pos, &cleaned);
        self.cursor_pos += cleaned.len();
        self.cursor.note_activity();
    }

    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
        self.cursor.note_activity();
    }
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.text.len();
        self.cursor.note_activity();
    }
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.text.floor_char_boundary(self.cursor_pos - 1);
            self.cursor.note_activity();
        }
    }
    pub fn cursor_right(&mut self) {
        let len = self.text.len();
        if self.cursor_pos < len {
            self.cursor_pos = self.text.floor_char_boundary(self.cursor_pos + 1).min(len);
            self.cursor.note_activity();
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
        self.cursor.note_activity();
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Always 3 — three-line input (centered text on middle line).
    pub const fn height(&self) -> u16 {
        3
    }

    /// Render the input as three lines with `┃` borders on both sides
    /// and `background_element` fill. Text and cursor live on line 1
    /// (the middle row) so they appear vertically centered.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16, width: u16, theme: &Theme) {
        if width < 4 {
            return;
        }

        let bg = rgba_color(theme.background_element);
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let border_color = rgba_color(theme.primary);
        let now = SystemTime::now();
        let right_x = x + width.saturating_sub(1);

        for row in 0..3 {
            let ry = y + row;

            // ── Left border ───────────────────────────────────────────
            if let Some(cell) = buf.cell_mut((x, ry)) {
                cell.set_char('\u{2503}');
                cell.set_style(Style::default().fg(border_color).bg(bg));
            }

            // ── Right border ──────────────────────────────────────────
            if let Some(cell) = buf.cell_mut((right_x, ry)) {
                cell.set_char('\u{2503}');
                cell.set_style(Style::default().fg(border_color).bg(bg));
            }

            // ── Background fill ───────────────────────────────────────
            for dx in 1..right_x.saturating_sub(x) {
                if let Some(cell) = buf.cell_mut((x + dx, ry)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(bg));
                }
            }
        }

        // ── Text / placeholder (centered vertically on line 1) ────────
        let text_y = y + 1; // middle row: line 0 empty, line 1 text, line 2 empty
        let text_x = x + 2;
        let text_w = width.saturating_sub(4) as usize;

        let is_placeholder = self.text.is_empty();
        let display_text = if is_placeholder {
            PLACEHOLDER
        } else {
            &self.text
        };
        let text_color = if is_placeholder { muted } else { fg };

        let truncated: String = display_text.chars().take(text_w).collect();
        for (i, ch) in truncated.chars().enumerate() {
            let cx = text_x + i as u16;
            if let Some(cell) = buf.cell_mut((cx, text_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(text_color).bg(bg));
            }
        }

        // ── Cursor (line 1 only, centered) ─────────────────────────────
        if self.is_focused {
            let cursor_col = if !self.text.is_empty() {
                self.text[..self.cursor_pos].chars().count()
            } else {
                0
            };
            let cursor_x = text_x + cursor_col as u16;
            if cursor_x < right_x {
                let state = self.cursor.current_state(now);
                if let Some(cell) = buf.cell_mut((cursor_x, text_y)) {
                    match state {
                        CursorState::On => {
                            cell.set_char('\u{2588}');
                            cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg));
                        }
                        CursorState::Off | CursorState::Blur => {
                            cell.set_char('\u{2592}');
                            cell.set_style(Style::default().fg(muted).bg(bg));
                        }
                    }
                }
            }
        }
    }
}

impl Default for RagInput {
    fn default() -> Self {
        Self::new()
    }
}
