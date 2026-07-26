use std::time::SystemTime;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::Theme;

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::unicode_util;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

const PLACEHOLDER: &str = "Enter URL or file path, then press Enter";

/// Maximum number of visible lines the input box can grow to.
const MAX_INPUT_HEIGHT: u16 = 15;

/// Minimum rows for the input box (placeholder or single short text).
const MIN_INPUT_HEIGHT: u16 = 3;

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

    /// Paste text into the input. Normalises line endings and strips all
    /// control characters (including `\n`) — the RAG input is single-line.
    pub fn handle_paste(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let cleaned: String = normalized.chars().filter(|c| !c.is_control()).collect();
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

    /// Map a screen-space mouse coordinate to a byte offset in `self.text`.
    /// `area` is the input box's position and size (the same `x`, `y`, `width`
    /// passed to `render()`). Returns `None` if the position is outside the
    /// text area or the input is empty.
    ///
    /// Uses the same grapheme-aware wrapping logic as `render()`, so the
    /// click-to-position behaviour matches the visual layout exactly.
    pub fn char_pos_at_mouse(&self, mouse_x: u16, mouse_y: u16, area: Rect) -> Option<usize> {
        if self.text.is_empty() {
            return None;
        }
        let text_w = (area.width.saturating_sub(4) as usize).max(1);
        let text_x = area.x + 2;
        let text_start = area.y + 1; // text starts at line 1 (line 0 = DB indicator)

        if mouse_y < text_start || mouse_x < text_x {
            return None;
        }

        let target_visual_line = (mouse_y - text_start) as usize;
        let target_col = (mouse_x - text_x) as usize;

        let mut current_line = 0usize;
        let mut col = 0u16;
        let mut byte_pos = 0usize;

        for (grapheme, gw) in unicode_util::graphemes_with_width(&self.text) {
            if col + gw > text_w as u16 {
                if current_line == target_visual_line {
                    // Past the last grapheme on the target line → return end of that line
                    return Some(byte_pos);
                }
                current_line += 1;
                col = 0;
            }

            if current_line > target_visual_line {
                // Beyond the target line → snap to end of text
                return Some(self.text.len());
            }

            if current_line == target_visual_line && (target_col as u16) < col + gw {
                return Some(byte_pos);
            }

            byte_pos += grapheme.len();
            col += gw;
        }

        // After all graphemes: click was on or past the last visual line
        Some(self.text.len())
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
        self.cursor.note_activity();
    }

    pub fn delete_word_before_cursor(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let start = crate::util::word_ops::find_word_start(&self.text, self.cursor_pos);
        if start < self.cursor_pos {
            self.cursor.note_activity();
            self.text.drain(start..self.cursor_pos);
            self.cursor_pos = start;
        }
    }

    pub fn cursor_word_left(&mut self) {
        if self.cursor_pos == 0 || self.text.is_empty() {
            return;
        }
        let new_pos = crate::util::word_ops::find_word_start(&self.text, self.cursor_pos);
        if new_pos < self.cursor_pos {
            self.cursor.note_activity();
            self.cursor_pos = new_pos;
        }
    }

    pub fn cursor_word_right(&mut self) {
        let len = self.text.len();
        if self.cursor_pos >= len || self.text.is_empty() {
            return;
        }
        let new_pos = crate::util::word_ops::find_word_end(&self.text, self.cursor_pos);
        if new_pos > self.cursor_pos {
            self.cursor.note_activity();
            self.cursor_pos = new_pos;
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of rows needed to display the input text at the given box width.
    /// Box width includes borders — internal text width is box_width - 4.
    /// Uses grapheme-aware display width so CJK/emoji don't cause overflow.
    /// Always keeps one empty row at the top for the DB indicator.
    /// Returns at least 3, capped at MAX_INPUT_HEIGHT.
    pub fn height(&self, box_width: u16) -> u16 {
        let text_w = (box_width.saturating_sub(4) as usize).max(1);
        if self.text.is_empty() {
            return MIN_INPUT_HEIGHT;
        }
        let total_w = unicode_util::str_display_width(&self.text);
        let text_lines = total_w.div_ceil(text_w);
        // +1 for the offset (text starts at line 1, line 0 reserved)
        let lines = text_lines + 1;
        (lines as u16).max(MIN_INPUT_HEIGHT).min(MAX_INPUT_HEIGHT)
    }

    /// Render the input box, drawing at most `max_height` rows.
    /// `width` is the full box width including borders.
    /// `max_height` caps the rendered height (e.g. when the layout has limited space).
    pub fn render(
        &self,
        buf: &mut Buffer,
        x: u16,
        y: u16,
        width: u16,
        max_height: u16,
        theme: &Theme,
    ) {
        if width < 4 {
            return;
        }

        let bg = rgba_color(theme.background_element);
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let border_color = rgba_color(theme.primary);
        let now = SystemTime::now();
        let right_x = x + width.saturating_sub(1);
        let desired_rows = self.height(width);
        let total_rows = desired_rows.min(max_height);
        let text_w = (width.saturating_sub(4) as usize).max(1);
        let text_x = x + 2;

        for row in 0..total_rows {
            let ry = y + row;
            if let Some(cell) = buf.cell_mut((x, ry)) {
                cell.set_char('\u{2503}');
                cell.set_style(Style::default().fg(border_color).bg(bg));
            }
            if let Some(cell) = buf.cell_mut((right_x, ry)) {
                cell.set_char('\u{2503}');
                cell.set_style(Style::default().fg(border_color).bg(bg));
            }
            for dx in 1..right_x.saturating_sub(x) {
                if let Some(cell) = buf.cell_mut((x + dx, ry)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(bg));
                }
            }
        }

        if self.text.is_empty() {
            let placeholder_line: String = PLACEHOLDER.chars().take(text_w).collect();
            let text_y = y + 1;
            for (i, ch) in placeholder_line.chars().enumerate() {
                let cx = text_x + i as u16;
                if let Some(cell) = buf.cell_mut((cx, text_y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(muted).bg(bg));
                }
            }
            if self.is_focused {
                let state = self.cursor.current_state(now);
                if let Some(cell) = buf.cell_mut((text_x, text_y)) {
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
        } else {
            // Text starts at line 1 (y+1), leaving line 0 reserved for the
            // DB indicator. Uses grapheme-aware iteration so that CJK, emoji,
            // and zero-width graphemes are handled safely without triggering
            // ratatui's set_char width assertion.
            // The loop stops early if `total_rows` is capped by the layout.
            let mut col = 0u16;
            let mut line = 1u16;
            for (grapheme, gw) in unicode_util::graphemes_with_width(&self.text) {
                if col + gw > text_w as u16 {
                    line += 1;
                    col = 0;
                }
                if line >= total_rows {
                    break;
                }
                let ry = y + line;
                let cx = text_x + col;
                if let Some(cell) = buf.cell_mut((cx, ry)) {
                    cell.set_symbol(grapheme);
                    cell.set_style(Style::default().fg(fg).bg(bg));
                }
                if gw > 1 {
                    for dx in 1..gw {
                        if let Some(cell) = buf.cell_mut((cx + dx, ry)) {
                            cell.set_diff_option(CellDiffOption::Skip);
                        }
                    }
                }
                col += gw;
            }

            // Cursor at line 1+offset, matching text position.
            if self.is_focused {
                let cursor_byte = self.cursor_pos.min(self.text.len());
                let prefix = &self.text[..cursor_byte];
                let mut acc_col = 0u16;
                let mut acc_line = 1u16;
                for (_, gw) in unicode_util::graphemes_with_width(prefix) {
                    if acc_col + gw > text_w as u16 {
                        acc_line += 1;
                        acc_col = 0;
                    }
                    acc_col += gw;
                }
                let cursor_col = acc_col;
                let cursor_line = acc_line;
                let cursor_y = y + cursor_line;
                let cursor_x = text_x + cursor_col;
                if cursor_line < total_rows && cursor_x < right_x {
                    let state = self.cursor.current_state(now);
                    if let Some(cell) = buf.cell_mut((cursor_x, cursor_y)) {
                        match state {
                            CursorState::On => {
                                cell.set_char('\u{2588}');
                                cell.set_style(
                                    Style::default().fg(rgba_color(theme.primary)).bg(bg),
                                );
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
}

impl Default for RagInput {
    fn default() -> Self {
        Self::new()
    }
}
