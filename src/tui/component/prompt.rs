use std::cell::Cell;
use std::time::SystemTime;

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::state::AppState;
use crate::theme::Theme;
use crate::types::{AgentColors, MessageRole, Part, Session};

const BASE_H: u16 = 2;
const AGENT_H: u16 = 1;
const CAP_H: u16 = 1;
const FOOTER_H: u16 = 1;
const PLACEHOLDER: &str = "Type a message...";

/// Thresholds for triggering paste compression (matches opencode: >=3 lines or >150 chars).
const PASTE_MIN_LINES: usize = 3;
const PASTE_MIN_CHARS: usize = 150;

/// A pasted text block that was compressed into a virtual-text placeholder.
#[derive(Clone, Debug)]
pub struct PastedPart {
    /// The short placeholder shown in the input (e.g. "[Pasted ~5 lines]").
    pub virtual_text: String,
    /// The original pasted text that will be expanded on submit/copy.
    pub actual_text: String,
}

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

const fn prompt_border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: '\u{2579}',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '\u{2503}',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '\u{2503}',
        right_t: ' ',
        cross: ' ',
    }
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

pub struct PromptView {
    pub input: String,
    pub cursor_pos: usize,
    pub input_text_width: Cell<usize>,
    pub history_index: i32,
    pub is_focused: bool,
    pub cursor: Cursor,
    pub sel_start: Option<usize>,
    pub sel_end: Option<usize>,
    /// Pasted text parts that were compressed into virtual-text placeholders.
    /// Each entry maps a placeholder like `[Pasted ~N lines]` to the original text.
    pub pasted_parts: Vec<PastedPart>,
}

impl PromptView {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            input_text_width: Cell::new(0),
            history_index: -1,
            is_focused: true,
            cursor: Cursor::new(),
            sel_start: None,
            sel_end: None,
            pasted_parts: Vec::new(),
        }
    }

    pub const fn focus(&mut self) {
        self.is_focused = true;
    }

    pub const fn blur(&mut self) {
        self.is_focused = false;
    }

    pub fn send_message(&mut self) -> String {
        let raw = self.input.clone();
        if !raw.is_empty() {
            // Expand any paste placeholders back to the original text before sending
            let msg = self.expand_pasted_text(&raw);
            self.input.clear();
            self.cursor_pos = 0;
            self.history_index = -1;
            self.pasted_parts.clear();
            msg
        } else {
            self.input.clear();
            self.cursor_pos = 0;
            self.history_index = -1;
            self.pasted_parts.clear();
            String::new()
        }
    }

    /// Navigate up through user messages from the current session.
    /// `user_msgs` should be the text of all user messages in the session,
    /// oldest first (index 0 = first message).
    pub fn history_up(&mut self, user_msgs: &[String]) {
        if user_msgs.is_empty() {
            return;
        }
        if self.history_index == -1 {
            self.history_index = i32::try_from(user_msgs.len()).unwrap_or(i32::MAX) - 1;
        } else if self.history_index > 0 {
            self.history_index -= 1;
        }
        self.input = user_msgs[usize::try_from(self.history_index).unwrap_or(0)].clone();
        self.cursor_pos = 0;
    }

    /// Navigate down through user messages from the current session.
    pub fn history_down(&mut self, user_msgs: &[String]) {
        if self.history_index == -1 {
            return;
        }
        self.history_index += 1;
        if self.history_index >= i32::try_from(user_msgs.len()).unwrap_or(i32::MAX) {
            self.history_index = -1;
            self.input.clear();
        } else {
            self.input = user_msgs[usize::try_from(self.history_index).unwrap_or(0)].clone();
        }
        self.cursor_pos = 0;
    }

    /// Reset history index so the next Ctrl+Down goes to the most recent
    /// history item instead of continuing from a previous browse position.
    /// Should be called whenever the user modifies the input directly
    /// (typing, backspace, delete, paste) after browsing history.
    pub fn reset_history_index(&mut self) {
        self.history_index = -1;
    }

    /// Extract the text content of all user messages from a session,
    /// in chronological order (oldest first). Used for history navigation.
    pub fn user_message_texts(session: &Session) -> Vec<String> {
        session
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .map(|m| {
                m.parts
                    .iter()
                    .filter_map(|p| {
                        if let Part::Text(t) = p {
                            Some(t.text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    pub fn note_activity(&mut self) {
        self.cursor.note_activity();
    }

    pub fn has_selection(&self) -> bool {
        self.sel_start.is_some() && self.sel_end.is_some() && self.sel_start != self.sel_end
    }

    pub fn selected_text(&self) -> String {
        match (self.sel_start, self.sel_end) {
            (Some(s), Some(e)) if s != e => {
                let start = s.min(e);
                let end = s.max(e);
                let end = end.min(self.input.len());
                let raw = self.input[start..end].to_string();
                // Expand any paste placeholders so the user copies the real content
                self.expand_pasted_text(&raw)
            }
            _ => String::new(),
        }
    }

    pub const fn clear_selection(&mut self) {
        self.sel_start = None;
        self.sel_end = None;
    }

    /// Backspace: if the cursor is within or at the end of a pasted virtual-text placeholder,
    /// delete the entire placeholder at once (matching opencode's atomic-virtual-text behaviour).
    /// Otherwise, delete a single character before the cursor.
    pub fn backspace(&mut self) {
        self.reset_history_index();
        self.note_activity();
        if self.cursor_pos == 0 {
            return;
        }
        let char_start = self.input.floor_char_boundary(self.cursor_pos - 1);
        // Check whether the character we are about to delete belongs to a pasted virtual text.
        if let Some((vt_pos, vt_end, idx)) =
            self.find_paste_overlapping(char_start, self.cursor_pos)
        {
            // Delete the ENTIRE virtual-text placeholder atomically.
            self.input.drain(vt_pos..vt_end);
            self.cursor_pos = vt_pos;
            self.pasted_parts.remove(idx);
            return;
        }
        // Normal backspace: delete one character before the cursor.
        self.input.remove(char_start);
        self.cursor_pos = char_start;
    }

    /// Delete (forward): if the cursor is within or at the start of a pasted virtual-text
    /// placeholder, delete the entire placeholder at once. Otherwise delete one character
    /// at the cursor position.
    pub fn delete(&mut self) {
        self.reset_history_index();
        self.note_activity();
        let len = self.input.len();
        if self.cursor_pos >= len {
            return;
        }
        let next = self.input.floor_char_boundary(self.cursor_pos + 1).min(len);
        // Check whether the character at the cursor belongs to a pasted virtual text.
        if let Some((vt_pos, vt_end, idx)) = self.find_paste_overlapping(self.cursor_pos, next) {
            // Delete the ENTIRE virtual-text placeholder atomically.
            self.input.drain(vt_pos..vt_end);
            if self.cursor_pos > vt_pos {
                self.cursor_pos = vt_pos;
            }
            self.pasted_parts.remove(idx);
            return;
        }
        // Normal forward-delete: delete one character at the cursor.
        self.input.drain(self.cursor_pos..next);
    }

    /// Returns `(byte_start, byte_end, part_index)` if any pasted virtual-text placeholder
    /// overlaps with the byte range `[start, end)` in `self.input`.
    fn find_paste_overlapping(&self, start: usize, end: usize) -> Option<(usize, usize, usize)> {
        for (idx, part) in self.pasted_parts.iter().enumerate() {
            if let Some(pos) = self.input.find(&part.virtual_text) {
                let vt_end = pos + part.virtual_text.len();
                if start < vt_end && end > pos {
                    return Some((pos, vt_end, idx));
                }
            }
        }
        None
    }

    /// Delete the word (or run of whitespace then word) immediately before the cursor.
    /// Behaves like Ctrl+Backspace in most terminals/editors.
    /// Handle pasted text. If the text is long (>=3 lines or >150 chars), compress it
    /// into a virtual-text placeholder like `[Pasted ~N lines]` and store the original
    /// text for later expansion. Otherwise, insert the text directly at the cursor.
    /// Matches the behaviour of opencode's `pasteInputText` + `pasteText`.
    pub fn handle_paste(&mut self, text: &str) {
        self.reset_history_index();
        // Normalize line endings (CRLF -> LF, CR -> LF), matching opencode
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let trimmed = normalized.trim().to_string();

        let line_count = trimmed.matches('\n').count() + 1;
        let is_long = line_count >= PASTE_MIN_LINES || trimmed.len() > PASTE_MIN_CHARS;

        if is_long {
            let virtual_text = format!("[Pasted ~{line_count} lines]");
            let actual_text = trimmed;

            // Insert virtual placeholder at cursor position
            let pos = self.cursor_pos;
            self.input.insert_str(pos, &virtual_text);
            self.cursor_pos = pos + virtual_text.len();

            // Store mapping for expansion on submit/copy
            self.pasted_parts.push(PastedPart {
                virtual_text,
                actual_text,
            });
        } else {
            // Short paste: insert text directly
            let pos = self.cursor_pos;
            self.input.insert_str(pos, &normalized);
            self.cursor_pos = pos + normalized.len();
        }
    }

    /// Expand any virtual-text placeholders in `input` back to their original pasted text.
    /// Applies replacements in insertion order (oldest first), matching opencode's
    /// `expandTrackedPastedText` semantics.
    pub fn expand_pasted_text(&self, input: &str) -> String {
        let mut result = input.to_string();
        for part in &self.pasted_parts {
            result = result.replace(&part.virtual_text, &part.actual_text);
        }
        result
    }

    pub fn delete_word_before_cursor(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let start = crate::util::word_ops::find_word_start(&self.input, self.cursor_pos);
        if start < self.cursor_pos {
            self.reset_history_index();
            self.note_activity();
            self.input.drain(start..self.cursor_pos);
            self.cursor_pos = start;
        }
    }

    pub fn cursor_word_left(&mut self) {
        if self.cursor_pos == 0 || self.input.is_empty() {
            return;
        }
        let new_pos = crate::util::word_ops::find_word_start(&self.input, self.cursor_pos);
        if new_pos < self.cursor_pos {
            self.note_activity();
            self.cursor_pos = new_pos;
        }
    }

    pub fn cursor_word_right(&mut self) {
        let len = self.input.len();
        if self.cursor_pos >= len || self.input.is_empty() {
            return;
        }
        let new_pos = crate::util::word_ops::find_word_end(&self.input, self.cursor_pos);
        if new_pos > self.cursor_pos {
            self.note_activity();
            self.cursor_pos = new_pos;
        }
    }

    pub fn cursor_up(&mut self, text_w: usize) {
        if self.input.is_empty() || text_w == 0 {
            return;
        }
        let lines: Vec<&str> = self.input.split('\n').collect();
        let cols = text_w.max(1);

        // char_pos counts actual Unicode characters from start of input to cursor_pos
        let char_pos = self.input[..self.cursor_pos].chars().count();
        let mut acc = 0usize; // accumulated character count (includes newlines)
        let mut byte_off = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let line_chars = line.chars().count();
            // Check if cursor falls on this logical line (chars + 1 for '\n' after it)
            if char_pos < acc + line_chars + 1 {
                let offset = char_pos.saturating_sub(acc);
                if offset >= line_chars {
                    // Cursor at end of this logical line.  Only treat it as a
                    // 'newline position' when there is actually a \n after this
                    // line (i.e. this is not the last logical line).  When it IS
                    // the last line the cursor is simply at the end of the text
                    // and we must fall through to visual-line navigation so
                    // wrapped lines work correctly.
                    if li + 1 < lines.len() {
                        // Cursor is at the newline between lines → go to
                        // previous logical line preserving visual column.
                        if li == 0 {
                            return;
                        }
                        let visual_col = offset % cols;
                        let prev_line = lines[li - 1];
                        let prev_chars = prev_line.chars().count();
                        let target_col = visual_col.min(cols.saturating_sub(1));
                        let target_char_idx = target_col.min(prev_chars.saturating_sub(1));
                        let prev_byte_start = byte_off - prev_line.len() - 1;
                        let target_byte = prev_line
                            .char_indices()
                            .nth(target_char_idx)
                            .map_or(prev_line.len(), |(i, _)| i);
                        self.cursor_pos = prev_byte_start + target_byte;
                        return;
                    }
                    // Last line, cursor at end → fall through to visual
                    // navigation within this same logical line.
                }
                let visual_line = offset / cols;
                if visual_line == 0 {
                    if li == 0 {
                        return; // already at top of text
                    }
                    // Move to previous logical line, preserving visual column.
                    // If the previous line is shorter, clamp to its end.
                    let visual_col = offset % cols;
                    let prev_line = lines[li - 1];
                    let prev_chars = prev_line.chars().count();
                    let target_col = visual_col.min(cols.saturating_sub(1));
                    let target_char_idx = target_col.min(prev_chars.saturating_sub(1));
                    let prev_byte_start = byte_off - prev_line.len() - 1;
                    let target_byte = prev_line
                        .char_indices()
                        .nth(target_char_idx)
                        .map_or(prev_line.len(), |(i, _)| i);
                    self.cursor_pos = prev_byte_start + target_byte;
                    return;
                }
                // Move up one visual line within the same logical line
                let visual_col = offset % cols;
                let target = (visual_line - 1) * cols + visual_col.min(cols.saturating_sub(1));
                let target_byte = line
                    .char_indices()
                    .nth(target)
                    .map_or(line.len(), |(i, _)| i);
                self.cursor_pos = byte_off + target_byte;
                return;
            }
            acc += line_chars + 1; // +1 for the '\n'
            byte_off += line.len() + 1; // +1 for '\n'
        }
    }

    pub fn cursor_down(&mut self, text_w: usize) {
        if self.input.is_empty() || text_w == 0 {
            return;
        }
        let lines: Vec<&str> = self.input.split('\n').collect();
        let cols = text_w.max(1);

        let char_pos = self.input[..self.cursor_pos].chars().count();
        let mut acc = 0usize;
        let mut byte_off = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let line_chars = line.chars().count();
            let visual_lines = line_chars.div_ceil(cols).max(1);
            if char_pos < acc + line_chars + 1 {
                let offset = char_pos.saturating_sub(acc);
                if offset >= line_chars {
                    // Cursor is at the newline → move to start of next line
                    if li + 1 >= lines.len() {
                        return; // already at bottom
                    }
                    let next_start = byte_off + line.len() + 1;
                    self.cursor_pos = next_start;
                    return;
                }
                let visual_line = offset / cols;
                let visual_col = offset % cols;
                if visual_line + 1 >= visual_lines {
                    // Last visual line of this logical line → move to next logical line
                    if li + 1 >= lines.len() {
                        return; // already at bottom
                    }
                    let next = lines[li + 1];
                    let next_chars = next.chars().count();
                    let target_col = visual_col.min(cols.saturating_sub(1));
                    let target_char_idx = target_col.min(next_chars.saturating_sub(1));
                    let next_start = byte_off + line.len() + 1;
                    self.cursor_pos = next_start
                        + next
                            .char_indices()
                            .nth(target_char_idx)
                            .map_or(0, |(i, _)| i);
                    return;
                }
                // Move down one visual line within the same logical line
                let target = (visual_line + 1) * cols + visual_col.min(cols.saturating_sub(1));
                let target_byte = line
                    .char_indices()
                    .nth(target)
                    .map_or(line.len(), |(i, _)| i);
                self.cursor_pos = byte_off + target_byte;
                return;
            }
            acc += line_chars + 1; // +1 for the '\n'
            byte_off += line.len() + 1; // +1 for '\n'
        }
    }

    pub fn required_height(&self, area_width: u16) -> u16 {
        let text_w = area_width.saturating_sub(5) as usize;
        let lines = if self.input.is_empty() || text_w == 0 {
            1
        } else {
            self.input
                .split('\n')
                .map(|line| {
                    let n = line.chars().count();
                    n.div_ceil(text_w).max(1)
                })
                .sum()
        };
        BASE_H + lines as u16 + AGENT_H + CAP_H + FOOTER_H
    }

    fn wrapped_lines(input: &str, max_w: usize) -> Vec<&str> {
        if input.is_empty() || max_w == 0 {
            return vec![input];
        }
        let mut result = Vec::new();
        for line in input.split('\n') {
            if line.is_empty() {
                result.push(line);
                continue;
            }
            let mut s = line;
            while !s.is_empty() {
                let line_len = s.chars().take(max_w).count();
                let split = s.char_indices().nth(line_len).map_or(s.len(), |(i, _)| i);
                result.push(&s[..split]);
                s = &s[split..];
            }
        }
        if result.is_empty() {
            result.push(input);
        }
        result
    }

    /// Map a screen-space mouse coordinate (x, y) to a character index in `self.input`.
    /// Returns `None` if the position is outside the text area of the prompt.
    pub fn char_pos_at_mouse(&self, x: u16, y: u16, area: Rect) -> Option<usize> {
        let text_w = area.width.saturating_sub(5) as usize;
        if text_w == 0 {
            return None;
        }
        let text_start = area.y + 1; // first content line
        let x_off = area.x + 3; // left margin within border
        let n = if self.input.is_empty() {
            1
        } else {
            self.input
                .split('\n')
                .map(|line| line.chars().count().div_ceil(text_w).max(1))
                .sum::<usize>() as u16
        };
        let bottom = text_start + n;

        if y < text_start || y >= bottom || x < x_off {
            return None;
        }

        let visual_line = (y - text_start) as usize;
        let col = (x - x_off) as usize;

        let display_lines = if self.input.is_empty() {
            vec![""]
        } else {
            Self::wrapped_lines(&self.input, text_w)
        };

        if visual_line >= display_lines.len() {
            return None;
        }

        let line = display_lines[visual_line];
        let col = col.min(line.chars().count());

        // Compute byte offset of the visual line in the original input.
        // Uses pointer arithmetic: wrapped_lines returns sub-slices of
        // self.input, so line.as_ptr() - input_start gives the exact byte
        // offset including \n separators between logical lines.
        // This is the same approach used in the render method for selection
        // highlighting (see render() ~line 680).
        let input_start = self.input.as_ptr() as usize;
        let byte_off = line.as_ptr() as usize - input_start;
        // Add the column within the visual line.
        if let Some((off, _)) = line.char_indices().nth(col) {
            Some(byte_off + off)
        } else {
            Some(byte_off + line.len())
        }
    }

    /// Render the prompt and draw a blinking cursor if focused.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        agent_colors: &AgentColors,
        unique_agents: &[String],
        now: SystemTime,
        model_name: &str,
    ) {
        let text_w = area.width.saturating_sub(5) as usize;
        self.input_text_width.set(text_w);
        let display_placeholder = self.input.is_empty();
        let display_text = if display_placeholder {
            PLACEHOLDER
        } else {
            &self.input
        };

        let display_lines = if display_text.is_empty() {
            vec![""]
        } else {
            Self::wrapped_lines(display_text, text_w)
        };
        let n = display_lines.len() as u16;

        let input_h = BASE_H + n + AGENT_H;
        let input_area = Rect::new(area.x, area.y, area.width, input_h);
        let cap_y = input_area.bottom();
        let cap_area = Rect::new(area.x, cap_y, area.width, CAP_H);
        let footer_y = cap_y + CAP_H;

        let agent_name = match state.mode {
            cosh::harness::Mode::Build => "build",
            cosh::harness::Mode::Ask => "ask",
        };

        let agent_color = match state.mode {
            cosh::harness::Mode::Build => agent_colors.get("build", unique_agents),
            cosh::harness::Mode::Ask => theme.info,
        };

        let mut border_box = BoxRenderable::new();
        border_box.set_border_color(Some(agent_color.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(prompt_border_chars());
        border_box.render_self(buf, input_area);

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_element.into()));
        let bg_area = Rect::new(
            input_area.x + 1,
            input_area.y,
            input_area.width.saturating_sub(1),
            input_area.height,
        );
        bg_box.render_self(buf, bg_area);

        let x_off = input_area.x + 3;
        let text_start = input_area.y + 1;
        let max_line_w = input_area.width.saturating_sub(5) as u16;

        // Pre-compute which visual lines overlap with pasted virtual-text placeholders.
        // Uses cumulative byte-offset tracking (same approach as `char_pos_at_mouse`)
        // instead of pointer arithmetic, which is fragile across string reallocations.
        let mut is_paste_line = vec![false; display_lines.len()];
        if !display_placeholder {
            let mut line_byte_start = 0usize;
            for (i, line) in display_lines.iter().enumerate() {
                let line_byte_end = line_byte_start + line.len();
                for part in &self.pasted_parts {
                    if let Some(vt_pos) = self.input.find(&part.virtual_text) {
                        let vt_end = vt_pos + part.virtual_text.len();
                        if line_byte_start < vt_end && line_byte_end > vt_pos {
                            is_paste_line[i] = true;
                            break;
                        }
                    }
                }
                line_byte_start = line_byte_end;
            }
        }

        for (i, line) in display_lines.iter().enumerate() {
            let ly = text_start + i as u16;
            if ly >= input_area.bottom() {
                break;
            }
            let base_style = if display_placeholder && i == 0 {
                Style::default().fg(rgba_color(theme.text_muted))
            } else {
                Style::default().fg(rgba_color(theme.text))
            };

            if is_paste_line[i] {
                draw_text_line(
                    buf,
                    line,
                    x_off,
                    ly,
                    max_line_w,
                    base_style.fg(rgba_color(theme.secondary)),
                );
            } else {
                draw_text_line(buf, line, x_off, ly, max_line_w, base_style);
            }
        }

        // ── Selection highlight ────────────────────────────────────────────────
        // Swap fg/bg on cells that fall within sel_start..sel_end so the user can
        // see which text they are dragging over.  Only applied when there is real
        // input text (not placeholder) and a non-empty selection range.
        if !display_placeholder
            && !self.input.is_empty()
            && let (Some(sel_s), Some(sel_e)) = (self.sel_start, self.sel_end)
            && sel_s != sel_e
        {
            let sel_a = sel_s.min(sel_e);
            let sel_b = sel_s.max(sel_e);
            let input_start = self.input.as_ptr() as usize;

            for (i, line) in display_lines.iter().enumerate() {
                let ly = text_start + i as u16;
                if ly >= input_area.bottom() {
                    break;
                }
                if line.is_empty() {
                    continue;
                }
                let line_byte_start = line.as_ptr() as usize - input_start;
                let line_byte_end = line_byte_start + line.len();

                // Check whether the selection overlaps this display line.
                if sel_a < line_byte_end && sel_b > line_byte_start {
                    let overlap_a = sel_a.max(line_byte_start);
                    let overlap_b = sel_b.min(line_byte_end);
                    if overlap_a >= overlap_b {
                        continue;
                    }

                    // Convert the within-line byte range to a column range.
                    let within_start = overlap_a - line_byte_start;
                    let within_end = overlap_b - line_byte_start;
                    let col_start = line[..within_start].chars().count();
                    let col_end = line[..within_end].chars().count();

                    for col in col_start..col_end {
                        let cx = x_off + col as u16;
                        if cx < input_area.right()
                            && let Some(cell) = buf.cell_mut((cx, ly))
                        {
                            let fg = cell.fg;
                            let bg = cell.bg;
                            cell.set_fg(bg);
                            cell.set_bg(fg);
                        }
                    }
                }
            }
        }

        let agent_label = capitalize(agent_name);
        let label_style = Style::default().fg(rgba_color(agent_color));
        let label_y = input_area.y + BASE_H + n;
        draw_text_line(buf, &agent_label, x_off, label_y, max_line_w, label_style);

        let mut cap_border_box = BoxRenderable::new();
        cap_border_box.set_border_color(Some(agent_color.into()));
        cap_border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: true,
        });
        cap_border_box.set_custom_border_chars(prompt_border_chars());
        cap_border_box.render_self(buf, cap_area);

        let cap_fill_x = cap_area.x + 1;
        let cap_fill_right = cap_area.right();
        let cap_style = Style::default()
            .fg(rgba_color(theme.background_element))
            .bg(rgba_color(theme.background));
        for cx in cap_fill_x..cap_fill_right {
            if let Some(cell) = buf.cell_mut((cx, cap_area.y)) {
                cell.set_char('\u{2580}');
                cell.set_style(cap_style);
            }
        }

        let muted_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(
            buf,
            "esc interrupt",
            area.x + 1,
            footer_y,
            area.width.saturating_sub(2),
            muted_style,
        );

        // Model name on the right side of the footer line
        if !model_name.is_empty() {
            let model_text = format!(" {model_name}");
            let model_x = area.right().saturating_sub(model_text.len() as u16);
            draw_text_line(
                buf,
                &model_text,
                model_x,
                footer_y,
                model_text.len() as u16,
                muted_style,
            );
        }

        // Draw cursor if focused
        if self.is_focused {
            let cursor_char = if display_placeholder || self.input.is_empty() {
                0
            } else {
                self.input[..self.cursor_pos].chars().count()
            };

            // Walk display_lines, accumulating char counts to find which
            // visual line and column the cursor falls on.
            // Each display line is a sub-slice of self.input (or the empty
            // string from split), so we can count chars directly.
            let mut cursor_line_idx = 0usize;
            let mut cursor_col_idx = 0usize;
            let mut acc = 0usize;

            for (li, line) in display_lines.iter().enumerate() {
                let n = line.chars().count();
                // Match cursor on this display line if its character position falls
                // within [acc, acc + n] (inclusive on both ends). The inclusive upper
                // bound handles the case where the cursor is at the newline after this
                // logical line — visually, that's the end of this display line.
                if cursor_char <= acc + n {
                    cursor_line_idx = li;
                    cursor_col_idx = cursor_char.saturating_sub(acc);
                    break;
                }
                acc += n;
                // If the next byte in self.input is \n, account for it
                if let Some((byte_idx, _)) = self.input.char_indices().nth(acc)
                    && byte_idx < self.input.len()
                    && self.input.as_bytes()[byte_idx] == b'\n'
                {
                    acc += 1;
                }
            }

            let cursor_y = text_start + cursor_line_idx as u16;
            if cursor_y < input_area.bottom() && cursor_line_idx < display_lines.len() {
                let cursor_x = x_off + cursor_col_idx as u16;
                if cursor_x < input_area.right()
                    && let Some(cell) = buf.cell_mut((cursor_x, cursor_y))
                {
                    // Use the reusable cursor component
                    match self.cursor.current_state(now) {
                        CursorState::On => {
                            // ON: transparent cursor (invert colors)
                            cell.set_style(
                                Style::default()
                                    .fg(rgba_color(theme.background))
                                    .bg(rgba_color(theme.text)),
                            );
                        }
                        CursorState::Off | CursorState::Blur => {
                            // OFF/Blur: dimmed visible state (not invisible)
                            cell.set_style(
                                Style::default()
                                    .fg(rgba_color(theme.text_muted))
                                    .bg(rgba_color(theme.background)),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
