use std::cell::Cell;
use std::time::SystemTime;

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::logo::ChatLogo;
use crate::lsp_colors;
use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::types::{AgentColors, MessageRole, Part, Session};

const BASE_H: u16 = 2;
const AGENT_H: u16 = 1;
const CAP_H: u16 = 1;
const FOOTER_H: u16 = 1;
const PLACEHOLDER: &str = "let's go!";

/// Maximum number of wrapped lines the prompt box may grow to before it stops
/// growing and starts scrolling its content upward. Generous by design: a chat
/// message can span several lines, but the input box must never take over the
/// whole terminal with unbounded text / newlines.
const MAX_PROMPT_LINES: usize = 20;

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

const fn prompt_border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: '\u{2579}',
        // Mirrors `bottom_left`: the right `┃` terminates with the same
        // up-stub on the cap row, keeping the box symmetric left/right.
        bottom_right: '\u{2579}',
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

/// Black or white depending on the background luminance, for readable text on
/// the colored language tags (mirrors the pending-queue row helper).
fn contrast_on(bg: RGBA) -> Color {
    let (r, g, b, _) = bg.to_ints();
    let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if lum > 128.0 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

/// Draw the inline LSP status segment on the prompt footer: an `LSP` label
/// followed by one colored tag per active server, or `disabled` when LSP is
/// turned off. Servers without a dedicated language color fall back to the
/// theme's primary color. Draws up to `max_w` columns starting at `x`,
/// returning the next free column.
fn draw_lsp_segment(
    buf: &mut Buffer,
    available: bool,
    servers: &[String],
    mut x: u16,
    y: u16,
    max_w: u16,
    theme: &Theme,
) -> u16 {
    let muted_style = Style::default().fg(rgba_color(theme.text_muted));
    let label = "LSP ";

    // "disabled" means LSP itself is off — not "no server running". When LSP
    // is on but nothing is running, just show the bare `LSP` label, no tags.
    if !available {
        let text = format!("{label} disabled");
        draw_text_line(buf, &text, x, y, max_w, muted_style);
        return x + text.chars().count() as u16;
    }

    for (i, ch) in label.chars().enumerate() {
        if x + i as u16 >= x + max_w {
            break;
        }
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(ch);
            cell.set_style(muted_style);
        }
    }
    x += label.chars().count() as u16;

    for server in servers {
        // The lookup gives the short human label (e.g. "Rust" for
        // `rust-analyzer`); unknown servers keep their raw name. Tag color
        // is the language's logo color, or theme.primary when unknown.
        let (label, bg) = match lsp_colors::lsp_tag(server) {
            Some((lbl, color)) => (lbl, color),
            None => (server.as_str(), theme.primary),
        };
        let fg = contrast_on(bg);
        let bg_color = rgba_color(bg);
        let tag = format!(" {label} ");
        for (i, ch) in tag.chars().enumerate() {
            let cx = x + i as u16;
            if cx >= x + max_w {
                return cx;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(fg).bg(bg_color));
            }
        }
        x += tag.chars().count() as u16;
    }
    x
}

/// Display name for a model: drop any lab/org prefix — e.g.
/// `deepseek.ai/deepseek-v4-pro` becomes `deepseek-v4-pro`. Display-only:
/// the configured model id keeps its full form for the model picker and API
/// calls. Note the lab prefix is unrelated to the gateway provider actually
/// being accessed (see `LlmConfig.provider`).
fn short_model_name(model: &str) -> &str {
    match model.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name,
        _ => model,
    }
}

/// Truncate `text` to at most `max` characters, ending with an ellipsis when
/// clipping occurs.
fn truncate_with_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('\u{2026}');
    clipped
}

/// Style for the reasoning-level badge: bold and color-coded by effort
/// intensity — green for low, amber for medium, red for high. Anything
/// unknown falls back to the theme's info color.
fn reason_level_style(level: &str, theme: &Theme) -> Style {
    let color = match level {
        "low" | "minimal" => theme.success,
        "medium" => theme.warning,
        "high" | "xhigh" => theme.error,
        _ => theme.info,
    };
    Style::default()
        .fg(rgba_color(color))
        .add_modifier(Modifier::BOLD)
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
    /// `true` while a right-button drag is selecting the whole prompt for
    /// model-assisted correction. The byte range is shared with ordinary
    /// copy selection; only its intent and visual treatment differ.
    pub correction_selection: bool,
    /// Pasted text parts that were compressed into virtual-text placeholders.
    /// Each entry maps a placeholder like `[Pasted ~N lines]` to the original text.
    pub pasted_parts: Vec<PastedPart>,
    /// The chat-logo animation (a single "O" with a red center and a laser beam).
    pub logo: ChatLogo,
    /// Snapshot of `(input.len(), cursor_pos)` from the previous frame, used to
    /// detect keystrokes so the logo's head-bob follows the typing rhythm.
    last_input_snapshot: Option<(usize, usize)>,
    /// Whether the logo was rendered in the previous frame (used to detect the
    /// landing-screen → chat transition so the animation can be reset).
    was_visible: bool,
    /// Index of the first visible wrapped line when the prompt content exceeds
    /// [`MAX_PROMPT_LINES`]. Kept from the last render so mouse hit-testing
    /// (`char_pos_at_mouse`) can map screen rows back to content lines.
    scroll_top: usize,
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
            correction_selection: false,
            pasted_parts: Vec::new(),
            logo: ChatLogo::new(),
            last_input_snapshot: None,
            was_visible: false,
            scroll_top: 0,
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
            let msg = self.expand_pasted_text(&raw).trim().to_string();
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
        self.cursor_pos = self.input.len();
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
        self.cursor_pos = self.input.len();
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

    /// Start a mouse selection. Both copy and correction use the same mature
    /// byte-offset selection machinery; `for_correction` only changes the
    /// highlight and what happens when the mouse button is released.
    pub fn begin_selection(&mut self, pos: usize, for_correction: bool) {
        self.cursor_pos = pos;
        self.sel_start = Some(pos);
        self.sel_end = Some(pos);
        self.correction_selection = for_correction;
    }

    /// A correction request is valid only when the drag covers every byte of
    /// the visible prompt. Mouse offsets are character boundaries, so this
    /// exact byte check also safely covers UTF-8 input.
    pub fn has_complete_correction_selection(&self) -> bool {
        if !self.correction_selection || self.input.is_empty() {
            return false;
        }
        matches!(
            (self.sel_start, self.sel_end),
            (Some(start), Some(end))
                if start.min(end) == 0 && start.max(end) == self.input.len()
        )
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
        self.correction_selection = false;
    }

    /// Replace the complete editable draft with a successful model
    /// correction. A correction is plain text, so any compressed-paste
    /// placeholders from the old draft are no longer applicable.
    pub fn replace_with_correction(&mut self, corrected: String) {
        self.input = corrected;
        self.cursor_pos = self.input.len();
        self.history_index = -1;
        self.pasted_parts.clear();
        self.clear_selection();
        self.note_activity();
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
    /// If the deletion range touches a pasted virtual-text placeholder, the ENTIRE
    /// placeholder is removed atomically (matching the atomic-virtual-text behaviour
    /// of `backspace`/`delete`) so Ctrl+Backspace / Ctrl+W cannot chip it away
    /// character by character, bypassing the atomic rule.
    pub fn delete_word_before_cursor(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let start = crate::util::word_ops::find_word_start(&self.input, self.cursor_pos);
        if start < self.cursor_pos {
            self.reset_history_index();
            self.note_activity();
            let mut from = start;
            let mut to = self.cursor_pos;
            // Grow the deletion range to cover every pasted virtual-text placeholder
            // it touches, then drop their mappings so the placeholder cannot be
            // deleted piecemeal by repeated Ctrl+Backspace / Backspace keystrokes.
            while let Some((vt_pos, vt_end, idx)) = self.find_paste_overlapping(from, to) {
                self.pasted_parts.remove(idx);
                from = from.min(vt_pos);
                to = to.max(vt_end);
            }
            self.input.drain(from..to);
            self.cursor_pos = from;
        }
    }

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
        let display_lines = Self::wrapped_lines(&self.input, text_w);
        if display_lines.len() <= 1 {
            return;
        }

        let input_start = self.input.as_ptr() as usize;

        // When the cursor sits on a '\n', treat it as being at the end of
        // the preceding display line (visually the newline is at line end).
        let at_newline =
            self.cursor_pos < self.input.len() && self.input.as_bytes()[self.cursor_pos] == b'\n';

        let mut cur_line = 0usize;
        let mut cur_col = 0usize;
        let mut found = false;

        for (i, line) in display_lines.iter().enumerate() {
            let line_start = line.as_ptr() as usize - input_start;
            let line_end = line_start + line.len();

            if at_newline && self.cursor_pos == line_end {
                cur_line = i;
                cur_col = line.chars().count();
                found = true;
                break;
            }

            if self.cursor_pos >= line_start && self.cursor_pos < line_end {
                cur_line = i;
                cur_col = self.input[line_start..self.cursor_pos].chars().count();
                found = true;
                break;
            }
        }

        if !found {
            // Cursor at the very end of input
            if let Some(last) = display_lines.last() {
                cur_line = display_lines.len() - 1;
                cur_col = last.chars().count();
            } else {
                return;
            }
        }

        if cur_line == 0 {
            return;
        }

        let prev_line = display_lines[cur_line - 1];
        let col = cur_col.min(prev_line.chars().count());
        let prev_start = prev_line.as_ptr() as usize - input_start;
        self.cursor_pos = prev_line
            .char_indices()
            .nth(col)
            .map_or(prev_start + prev_line.len(), |(i, _)| prev_start + i);
    }

    pub fn cursor_down(&mut self, text_w: usize) {
        if self.input.is_empty() || text_w == 0 {
            return;
        }
        let display_lines = Self::wrapped_lines(&self.input, text_w);
        if display_lines.is_empty() {
            return;
        }

        let input_start = self.input.as_ptr() as usize;

        let at_newline =
            self.cursor_pos < self.input.len() && self.input.as_bytes()[self.cursor_pos] == b'\n';

        let mut cur_line = 0usize;
        let mut cur_col = 0usize;
        let mut found = false;

        for (i, line) in display_lines.iter().enumerate() {
            let line_start = line.as_ptr() as usize - input_start;
            let line_end = line_start + line.len();

            if at_newline && self.cursor_pos == line_end {
                // At '\n' → visually at start of next line, column 0
                if i + 1 < display_lines.len() {
                    cur_line = i + 1;
                    cur_col = 0;
                    found = true;
                }
                break;
            }

            if self.cursor_pos >= line_start && self.cursor_pos < line_end {
                cur_line = i;
                cur_col = self.input[line_start..self.cursor_pos].chars().count();
                found = true;
                break;
            }
        }

        if !found {
            return;
        }

        if cur_line + 1 >= display_lines.len() {
            return;
        }

        let next_line = display_lines[cur_line + 1];
        let col = cur_col.min(next_line.chars().count());
        let next_start = next_line.as_ptr() as usize - input_start;
        self.cursor_pos = next_line
            .char_indices()
            .nth(col)
            .map_or(next_start + next_line.len(), |(i, _)| next_start + i);
    }

    /// Compute the prompt's height. `max_height` is the vertical budget the
    /// layout can afford (the available screen space above the footer), so the
    /// box never overflows a small terminal: the limit is **responsive** — a
    /// short screen caps the box low right away, while a tall screen lets it
    /// stretch up to the generous [`MAX_PROMPT_LINES`] hard cap.
    pub fn required_height(&self, area_width: u16, max_height: u16) -> u16 {
        // 6 columns of horizontal chrome: border + 2 padding on each side.
        let text_w = area_width.saturating_sub(6) as usize;
        let content_lines = if self.input.is_empty() || text_w == 0 {
            1
        } else {
            Self::wrapped_lines(&self.input, text_w).len()
        };
        let overhead = BASE_H + AGENT_H + CAP_H + FOOTER_H;
        let budget_lines = max_height.saturating_sub(overhead).max(1);
        let lines = content_lines
            .min(MAX_PROMPT_LINES)
            .min(budget_lines as usize);
        overhead + lines as u16
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
            let mut remaining = line;
            while !remaining.is_empty() {
                let char_count = remaining.chars().count();
                if char_count <= max_w {
                    result.push(remaining);
                    break;
                }
                // Find the last space within the first max_w characters
                // so we can break at a word boundary.
                let mut last_space = None;
                let mut count = 0;
                for (i, ch) in remaining.char_indices() {
                    if ch == ' ' {
                        last_space = Some(i);
                    }
                    count += 1;
                    if count >= max_w {
                        break;
                    }
                }
                if let Some(sp) = last_space {
                    // Break after the space (space goes on current line)
                    let end = sp + 1;
                    result.push(&remaining[..end]);
                    remaining = &remaining[end..];
                } else {
                    // No space found within max_w chars — fall back to char break
                    // (handles very long words without spaces)
                    let split = remaining
                        .char_indices()
                        .nth(max_w)
                        .map_or(remaining.len(), |(i, _)| i);
                    result.push(&remaining[..split]);
                    remaining = &remaining[split..];
                }
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
        if self.input.is_empty() {
            return None;
        }
        let text_w = area.width.saturating_sub(6) as usize;
        if text_w == 0 {
            return None;
        }
        let text_start = area.y + 1; // first content line
        let x_off = area.x + 3; // left margin within border
        let n = Self::wrapped_lines(&self.input, text_w).len();
        // The visible box is capped responsively (derive the same window as the
        // render does from the allocated `area.height`); clicks can only land on
        // the scrolled-in window, so remap the row to the content line using the
        // scroll offset from the last render.
        let eff_cap = area
            .height
            .saturating_sub(BASE_H + AGENT_H + CAP_H + FOOTER_H)
            .max(1);
        let bottom = text_start + n.min(eff_cap as usize) as u16;

        if y < text_start || y >= bottom || x < x_off {
            return None;
        }

        let visual_line = self.scroll_top + (y - text_start) as usize;
        let col = (x - x_off) as usize;

        let display_lines = Self::wrapped_lines(&self.input, text_w);

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

    /// Render the prompt and draw the cursor — blinking when focused, dimmed when blurred.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        agent_colors: &AgentColors,
        unique_agents: &[String],
        now: SystemTime,
        provider: &str,
        model_name: &str,
        reasoning: Option<&str>,
        delta_time: f64,
        show_logo: bool,
    ) {
        // 6 columns of horizontal chrome: border + 2 padding on each side.
        let text_w = area.width.saturating_sub(6) as usize;
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

        // Locate the cursor's visual (wrapped) line/column so the scroll window
        // below can always keep it visible.
        let (cursor_line_idx, cursor_col_idx) = if display_placeholder || self.input.is_empty() {
            (0usize, 0usize)
        } else {
            let input_start = self.input.as_ptr() as usize;
            let at_newline = self.cursor_pos < self.input.len()
                && self.input.as_bytes()[self.cursor_pos] == b'\n';
            let mut result = (0usize, 0usize);
            for (i, line) in display_lines.iter().enumerate() {
                let line_start = line.as_ptr() as usize - input_start;
                let line_end = line_start + line.len();
                if at_newline && self.cursor_pos == line_end {
                    result = (i, line.chars().count());
                    break;
                }
                if self.cursor_pos >= line_start && self.cursor_pos < line_end {
                    result = (i, self.input[line_start..self.cursor_pos].chars().count());
                    break;
                }
                if i + 1 == display_lines.len() && self.cursor_pos >= line_end {
                    result = (i, self.input[line_start..self.cursor_pos].chars().count());
                    break;
                }
            }
            result
        };

        // Growth limit + scroll-up window. The box height is already capped
        // responsively by `required_height` (`area.height` here); derive the
        // visible line count from it so a small screen caps the window low and
        // a tall screen allows up to `MAX_PROMPT_LINES`. When the content
        // overflows, only the window's lines ending at the cursor are drawn
        // (the older top lines visually leave the box upward as new lines
        // arrive — an LRU-like scroll-up, like the chat's summarizing box).
        let total_lines = n as usize;
        let eff_cap = area
            .height
            .saturating_sub(BASE_H + AGENT_H + CAP_H + FOOTER_H)
            .max(1);
        let eff_n = n.min(eff_cap);
        let scroll_top = prompt_scroll_top(cursor_line_idx, total_lines, eff_n as usize);
        self.scroll_top = scroll_top;

        let input_h = BASE_H + eff_n + AGENT_H;
        let input_area = Rect::new(area.x, area.y, area.width, input_h);
        let cap_y = input_area.bottom();
        let cap_area = Rect::new(area.x, cap_y, area.width, CAP_H);
        let footer_y = cap_y + CAP_H;

        let agent_name = match state.mode {
            cosh::harness::Mode::Build => "build",
            cosh::harness::Mode::Ask => "ask",
            cosh::harness::Mode::Yolo => "yolo",
        };

        let agent_color = match state.mode {
            cosh::harness::Mode::Build => agent_colors.get("build", unique_agents),
            cosh::harness::Mode::Ask => theme.info,
            cosh::harness::Mode::Yolo => theme.warning,
        };

        let mut border_box = BoxRenderable::new();
        border_box.set_border_color(Some(agent_color.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: true,
            bottom: false,
        });
        border_box.set_custom_border_chars(prompt_border_chars());
        border_box.render_self(buf, input_area);

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_element.into()));
        // The band stops one column short of the right edge so the right `┃`
        // sits on the terminal background, mirroring the left border (this
        // also matches the slash menu's content band exactly).
        let bg_area = Rect::new(
            input_area.x + 1,
            input_area.y,
            input_area.width.saturating_sub(2),
            input_area.height,
        );
        bg_box.render_self(buf, bg_area);

        let x_off = input_area.x + 3;
        let text_start = input_area.y + 1;
        // Text width: left chrome (border + 2 padding = 3) and right chrome
        // (2 padding + border = 3) — symmetric now that the box has a right
        // `┃` like the slash menu.
        let max_line_w = input_area.width.saturating_sub(6) as u16;

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

        for (idx, line) in display_lines.iter().enumerate().skip(scroll_top) {
            let ly = text_start + (idx - scroll_top) as u16;
            if ly >= input_area.bottom() {
                break;
            }
            let base_style = if display_placeholder && idx == 0 {
                Style::default().fg(rgba_color(theme.text_muted))
            } else {
                Style::default().fg(rgba_color(theme.text))
            };

            if is_paste_line[idx] {
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

            for (idx, line) in display_lines.iter().enumerate().skip(scroll_top) {
                let ly = text_start + (idx - scroll_top) as u16;
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
                            if self.correction_selection {
                                // Use the active theme's accent as a vivid,
                                // unmistakably different selection intended
                                // for correction. The prompt's normal copy
                                // selection remains the familiar inversion.
                                cell.set_fg(contrast_on(theme.accent));
                                cell.set_bg(rgba_color(theme.accent));
                            } else {
                                let fg = cell.fg;
                                let bg = cell.bg;
                                cell.set_fg(bg);
                                cell.set_bg(fg);
                            }
                        }
                    }
                }
            }
        }

        // Paint the label row (`Build … provider model . high`) with the box
        // fill before drawing its text: the row sits inside the input box, so
        // its cells must carry the exact `background_element` color — stale
        // glyphs left by input lines that previously occupied this row would
        // otherwise stay visible through the fg-only styles below.
        // Mirrors the cap-band rule: on transparent themes (`background_element`
        // alpha 0 → `Color::Reset`) no fill is painted, only the glyph clear.
        let label_y = input_area.y + BASE_H + eff_n;
        let label_bg = rgba_color(theme.background_element);
        for cx in x_off..x_off + max_line_w {
            if let Some(cell) = buf.cell_mut((cx, label_y)) {
                cell.set_char(' ');
                if label_bg != Color::Reset {
                    cell.set_bg(label_bg);
                }
            }
        }
        // Row-scoped styles: every segment drawn on the label row carries the
        // box fill as its background so the text always reads over it.
        let row_bg = if label_bg != Color::Reset {
            Style::default().bg(label_bg)
        } else {
            Style::default()
        };
        let row_muted_style = row_bg.fg(rgba_color(theme.text_muted));

        let agent_label = capitalize(agent_name);
        let label_style = row_bg.fg(rgba_color(agent_color));
        draw_text_line(buf, &agent_label, x_off, label_y, max_line_w, label_style);

        let mut cap_border_box = BoxRenderable::new();
        cap_border_box.set_border_color(Some(agent_color.into()));
        cap_border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: true,
            bottom: true,
        });
        cap_border_box.set_custom_border_chars(prompt_border_chars());
        cap_border_box.render_self(buf, cap_area);

        let cap_fill_x = cap_area.x + 1;
        // Stop one column early so the `▀` band never covers the right `╹`
        // termination of the right border (mirrors the left `╹`).
        let cap_fill_right = cap_area.right().saturating_sub(1);
        // Only paint the blend band when the fill is a real color. On
        // transparent themes (`orng`; anywhere `background_element` has
        // alpha 0) `rgba_color` maps the fill to `Color::Reset`, and a `▀`
        // painted with a Reset foreground renders its upper half in the
        // terminal's *default* fg (usually white) — a bright band the input
        // box never shows (`BoxRenderable` skips alpha-0 fills entirely).
        // Mirror that "alpha 0 = no fill" rule here: skip the glyph so the
        // cap row stays as transparent as the box fill itself.
        if rgba_color(theme.background_element) != Color::Reset {
            let cap_style = Style::default()
                .fg(rgba_color(theme.background_element))
                .bg(rgba_color(theme.background));
            for cx in cap_fill_x..cap_fill_right {
                if let Some(cell) = buf.cell_mut((cx, cap_area.y)) {
                    cell.set_char('\u{2580}');
                    cell.set_style(cap_style);
                }
            }
        }

        let muted_style = Style::default().fg(rgba_color(theme.text_muted));
        // The LSP status only appears after the first user message lowers the
        // box from its initial position — while the session is empty (input
        // still raised) the footer shows only the mode hint.
        let has_conversation = state
            .current_session()
            .is_some_and(|s| !s.messages.is_empty());
        let footer_text = if has_conversation {
            "esc interrupt"
        } else {
            "tab change mode"
        };
        let mut footer_x = area.x + 1;
        let footer_max_w = area.width.saturating_sub(2);
        draw_text_line(
            buf,
            footer_text,
            footer_x,
            footer_y,
            footer_max_w,
            muted_style,
        );
        footer_x = footer_x.saturating_add(footer_text.chars().count() as u16);

        // Inline LSP status: ` LSP <tag> <tag>…`, tags painted with the
        // language's logo color (theme.primary fallback), disabled when empty.
        // Omitted entirely while the input box is still raised (no message yet).
        if has_conversation {
            footer_x = footer_x.saturating_add(3);
            let remaining = footer_max_w.saturating_sub(footer_x.saturating_sub(area.x));
            draw_lsp_segment(
                buf,
                state.lsp_available,
                &state.lsp_servers,
                footer_x,
                footer_y,
                remaining,
                theme,
            );
        }

        // Provider + model + reasoning level on the agent-label row — the
        // same line as Build/Ask/Yolo, inside the input box, right-aligned.
        // Layout is `provider model . level`, e.g.
        // ` openrouter deepseek-v4-flash . high`. The provider is the
        // gateway/API actually being accessed (from `LlmConfig.provider`,
        // unrelated to the model id's lab prefix) and stays muted so it reads
        // as a qualifier next to the bright model name, which uses the
        // theme's adaptive text color (a white tone on dark themes,
        // near-black on light ones such as sakura). The reasoning level is
        // bold and color-coded by effort intensity; the ` . ` separator is
        // muted too. Segments are truncated so the row always respects the
        // box interior and never overlaps the agent label on the left.
        if !model_name.is_empty() {
            // Bare model name, without the lab/org prefix (display only).
            let model_name = short_model_name(model_name);
            let reason = reasoning.filter(|r| !r.is_empty() && *r != "default");
            let sep = if reason.is_some() { " . " } else { "" };
            let model_style = row_bg.fg(rgba_color(theme.text));
            let reason_style = reason
                .map(|r| row_bg.patch(reason_level_style(r, theme)))
                .unwrap_or(row_muted_style);

            // Gateway provider prefix, muted, followed by one spacer column.
            let prov_seg = if provider.is_empty() {
                String::new()
            } else {
                format!("{provider} ")
            };
            let prov_w = prov_seg.chars().count();
            let sep_w = sep.chars().count();
            let reason_w = reason.map(|r| r.chars().count()).unwrap_or(0);
            let model_w = model_name.chars().count();

            // Interior width mirrors `max_line_w`: 3 columns of chrome on
            // each side (border + 2 padding). Reserve room for the agent
            // label plus one spacer column.
            let interior_w = input_area.width.saturating_sub(6) as usize;
            let label_w = agent_label.chars().count() + 1;
            let avail = interior_w.saturating_sub(label_w);

            // Fit order: drop the provider first, then shrink the model name
            // (ellipsis) — the reasoning level always stays fully visible.
            // When even the level alone would not fit, show only the level.
            let reason_text = reason.unwrap_or_default().to_string();
            let (prov_out, model_out, reason_out) = if prov_w + model_w + sep_w + reason_w <= avail
            {
                (prov_seg, model_name.to_string(), reason_text)
            } else if model_w + sep_w + reason_w <= avail {
                (String::new(), model_name.to_string(), reason_text)
            } else if avail > sep_w + reason_w {
                (
                    String::new(),
                    truncate_with_ellipsis(model_name, avail - sep_w - reason_w),
                    reason_text,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    reason
                        .map(|r| truncate_with_ellipsis(r, avail))
                        .unwrap_or_default(),
                )
            };

            let mut segments: Vec<(String, Style)> = Vec::new();
            if !prov_out.is_empty() {
                segments.push((prov_out, row_muted_style));
            }
            if !model_out.is_empty() {
                segments.push((model_out, model_style));
            }
            if !reason_out.is_empty() {
                segments.push((sep.to_string(), row_muted_style));
                segments.push((reason_out, reason_style));
            }
            let total_w: usize = segments.iter().map(|(t, _)| t.chars().count()).sum();
            if total_w > 0 {
                let mut x = input_area.right().saturating_sub(3 + total_w as u16);
                for (text, style) in segments {
                    let w = text.chars().count() as u16;
                    if w > 0 {
                        draw_text_line(buf, &text, x, label_y, w, style);
                        x += w;
                    }
                }
            }
        }

        // ── Chat logo animation ───────────────────────────────────────────
        // The O's center turns red, then a laser beams toward the typing
        // position. The O drifts toward the cursor with a head-like motion.
        // Only rendered on the empty-session landing screen where a logo slot
        // is reserved above the prompt.
        // `cursor_line_idx`/`cursor_col_idx` were computed above when the scroll
        // window was established; only the cursor's screen row needs the
        // scroll offset subtracted so it stays inside the visible window.
        let cursor_y = text_start + (cursor_line_idx - scroll_top) as u16;
        let cursor_x = x_off + cursor_col_idx as u16;

        if show_logo {
            // Detect a keystroke since the previous frame (input changed) and
            // feed it to the logo so its head-bob follows the typing rhythm.
            let snapshot = (self.input.len(), self.cursor_pos);
            if self.last_input_snapshot != Some(snapshot) {
                self.logo.note_keystroke(cursor_x, cursor_y);
            }
            self.last_input_snapshot = Some(snapshot);

            // Advance the animation toward the typing position, then draw it.
            // The logo band sits just above the prompt box. The O glyph is
            // clamped inside this band (it never enters the prompt), while the
            // laser beam may cross the bottom edge and reach into the prompt
            // toward the cursor.
            let logo_x = input_area.x + input_area.width / 2;
            let logo_y = input_area.y.saturating_sub(5);
            let logo_area = Rect::new(logo_x.saturating_sub(4), logo_y, input_area.width, 5);
            self.logo.anchor(logo_area);
            self.logo
                .advance(delta_time, cursor_x as f64, cursor_y as f64);
            self.logo.render(
                buf,
                logo_area,
                cursor_x,
                cursor_y,
                rgba_color(theme.primary),
                rgba_color(theme.background),
            );
            self.was_visible = true;
        } else if self.was_visible {
            // The landing screen is gone (message sent): reset the animation
            // so it re-fills/re-fires when the next empty session appears.
            self.was_visible = false;
            self.last_input_snapshot = None;
            self.logo.reset();
        }

        if cursor_y < input_area.bottom()
            && cursor_line_idx < display_lines.len()
            && cursor_x < input_area.right()
            && let Some(cell) = buf.cell_mut((cursor_x, cursor_y))
        {
            let dimmed_style = Style::default()
                .fg(rgba_color(theme.text_muted))
                .bg(rgba_color(theme.background));
            if self.is_focused {
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
                        // OFF/Blur: dimmed visible state
                        cell.set_style(dimmed_style);
                    }
                }
            } else {
                // Unfocused: always show dimmed
                cell.set_style(dimmed_style);
            }
        }
    }
}

/// Index of the first visible wrapped line when the prompt content exceeds the
/// visible window (`window` is the responsive line cap — capped low on small
/// screens, up to [`MAX_PROMPT_LINES`] on tall ones). The window keeps the
/// cursor line visible: normally it sits at the bottom of the window (so new
/// lines push old ones off the top — the scroll-up effect), and it follows the
/// cursor if the user moves it up.
fn prompt_scroll_top(cursor_line: usize, total_lines: usize, window: usize) -> usize {
    if total_lines <= window {
        return 0;
    }
    let window = window as isize;
    let cursor_line = cursor_line as isize;
    let desired = (cursor_line - window + 1).max(0);
    (desired.min(total_lines as isize - window)).max(0) as usize
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_height_caps_at_generous_limit() {
        let view = PromptView::new();
        // One wrapped line at width 200, unbounded budget.
        let base = view.required_height(200, 1000);
        // A single line: 1 wrapped line → BASE_H + 1 + AGENT_H + CAP_H + FOOTER_H.
        assert_eq!(base, BASE_H + 1 + AGENT_H + CAP_H + FOOTER_H);

        // Many newlines must not grow the box past the hard cap (tall screen).
        let mut huge = PromptView::new();
        huge.input = "line\n".repeat(500);
        assert_eq!(
            huge.required_height(200, 1000),
            BASE_H + MAX_PROMPT_LINES as u16 + AGENT_H + CAP_H + FOOTER_H,
            "required_height must be capped at MAX_PROMPT_LINES"
        );
    }

    #[test]
    fn required_height_respects_vertical_budget_on_small_screens() {
        let overhead = BASE_H + AGENT_H + CAP_H + FOOTER_H;
        let mut view = PromptView::new();
        view.input = "line\n".repeat(500);

        // A compressed screen hands a small budget: the box shrinks right away
        // instead of staying at the hard cap (which would overflow the screen).
        let small_budget = overhead + 3;
        assert_eq!(
            view.required_height(200, small_budget),
            overhead + 3,
            "small budget must cap the prompt low immediately"
        );

        // A large budget still hits the generous hard cap.
        assert_eq!(
            view.required_height(200, 1000),
            overhead + MAX_PROMPT_LINES as u16,
            "generous budget keeps the tall-screen behavior"
        );

        // Even when the budget is smaller than the box's fixed overhead, it
        // never collapses below a single usable line.
        assert_eq!(
            view.required_height(200, overhead.saturating_sub(1)),
            overhead + 1,
            "never collapse below one line"
        );
    }

    #[test]
    fn scroll_top_stays_zero_within_limit() {
        for total in 0..=MAX_PROMPT_LINES {
            assert_eq!(prompt_scroll_top(total, total, MAX_PROMPT_LINES), 0);
        }
        // Cursor at top of a fitting buffer.
        assert_eq!(prompt_scroll_top(0, MAX_PROMPT_LINES, MAX_PROMPT_LINES), 0);
    }

    #[test]
    fn scroll_top_keeps_bottom_visible_when_overflowing() {
        // Cursor at the last line → the last `window` lines are shown.
        let window = MAX_PROMPT_LINES;
        let total = MAX_PROMPT_LINES + 10;
        assert_eq!(
            prompt_scroll_top(total - 1, total, window),
            total - window,
            "bottom lines stay visible (scroll-up effect)"
        );
        // Cursor at the very top → the first lines are shown instead.
        assert_eq!(
            prompt_scroll_top(0, total, window),
            0,
            "cursor at top keeps the window at the top"
        );
        // Cursor in the middle → window follows so the cursor line is visible.
        let cursor = MAX_PROMPT_LINES + 4;
        let st = prompt_scroll_top(cursor, total, window);
        assert!(cursor >= st && cursor < st + window);
    }

    #[test]
    fn scroll_top_respects_a_small_responsive_window() {
        // Small screen → small window: overflow kicks in with far fewer lines.
        let window = 4;
        let total = 10;
        assert_eq!(prompt_scroll_top(total - 1, total, window), total - window);
        assert_eq!(prompt_scroll_top(0, total, window), 0);
    }

    #[test]
    fn correction_selection_requires_the_complete_prompt() {
        let mut view = PromptView::new();
        view.input = "Olá, mundó".into();

        // Selecting all but the final UTF-8 character is still incomplete.
        view.begin_selection(0, true);
        let last_char = view.input.floor_char_boundary(view.input.len() - 1);
        view.sel_end = Some(last_char);
        assert!(!view.has_complete_correction_selection());

        // Reversing the drag direction must still count as a complete range.
        view.begin_selection(view.input.len(), true);
        view.sel_end = Some(0);
        assert!(view.has_complete_correction_selection());

        view.clear_selection();
        assert!(!view.correction_selection);
        assert!(!view.has_complete_correction_selection());
    }

    #[test]
    fn successful_correction_replaces_plain_text_and_drops_paste_mappings() {
        let mut view = PromptView::new();
        view.handle_paste("first\nsecond\nthird");
        assert!(!view.pasted_parts.is_empty());

        view.replace_with_correction("First, second, and third.".into());

        assert_eq!(view.input, "First, second, and third.");
        assert_eq!(view.cursor_pos, view.input.len());
        assert!(view.pasted_parts.is_empty());
        assert!(!view.has_selection());
    }

    #[test]
    fn correction_selection_uses_the_theme_accent_background() {
        use crate::theme::ThemeRegistry;

        let theme = ThemeRegistry::new().default_theme().clone();
        let state = AppState::new();
        let mut view = PromptView::new();
        view.input = "abc".into();
        view.cursor_pos = view.input.len();
        view.begin_selection(0, true);
        view.sel_end = Some(view.input.len());

        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        view.render(
            &mut buf,
            area,
            &state,
            &theme,
            &AgentColors::from_theme(&theme),
            &[],
            SystemTime::now(),
            "",
            "",
            None,
            0.0,
            false,
        );

        // Prompt text starts three columns inside the left border.
        assert_eq!(buf[(3, 1)].bg, rgba_color(theme.accent));
    }

    #[test]
    fn cap_fill_skips_transparent_element_but_keeps_opaque_blend() {
        use crate::theme::ThemeRegistry;

        let registry = ThemeRegistry::new();
        // Transparent theme (`orng`): background_element has alpha 0, so the
        // cap `▀` band must NOT be painted — mirroring BoxRenderable's
        // "alpha 0 = no fill" rule. Painting it would render the glyph's
        // upper half in `Color::Reset` (terminal default, typically white).
        let orng = registry.get("orng").expect("orng theme registered");
        let state = AppState::new();

        let area = Rect::new(0, 0, 80, 10);
        // input_area.y + input_h (one content line) = cap row.
        let cap_y = BASE_H + 1 + AGENT_H;

        let mut transparent_view = PromptView::new();
        let mut buf = Buffer::empty(area);
        transparent_view.render(
            &mut buf,
            area,
            &state,
            orng,
            &AgentColors::from_theme(orng),
            &[],
            SystemTime::now(),
            "",
            "",
            None,
            0.0,
            false,
        );
        for x in 1..area.right() - 1 {
            let cell = &buf[(x, cap_y)];
            assert_ne!(
                cell.symbol(),
                "\u{2580}",
                "cap row on a transparent theme must stay glyph-free (x={x})"
            );
        }

        // Opaque themes keep the visual intent: the `▀` blend band is still
        // painted as the smooth gradient between box fill and background.
        let cosh = registry.default_theme();
        let mut opaque_view = PromptView::new();
        let mut buf = Buffer::empty(area);
        opaque_view.render(
            &mut buf,
            area,
            &state,
            cosh,
            &AgentColors::from_theme(cosh),
            &[],
            SystemTime::now(),
            "",
            "",
            None,
            0.0,
            false,
        );
        for x in 1..area.right() - 1 {
            let cell = &buf[(x, cap_y)];
            assert_eq!(
                cell.symbol(),
                "\u{2580}",
                "opaque themes must keep painting the blend band (x={x})"
            );
        }
    }

    #[test]
    fn ctrl_backspace_deletes_pasted_placeholder_atomically() {
        let mut view = PromptView::new();
        view.input = "hey ".into();
        view.cursor_pos = view.input.len();
        view.handle_paste("line one\nline two\nline three");
        assert_eq!(view.input, "hey [Pasted ~3 lines]");
        assert_eq!(view.pasted_parts.len(), 1);

        // Ctrl+Backspace (delete word before cursor) must remove the WHOLE
        // placeholder in one keystroke, leaving no orphan mapping behind.
        view.delete_word_before_cursor();
        assert_eq!(view.input, "hey ");
        assert_eq!(view.cursor_pos, "hey ".len());
        assert!(view.pasted_parts.is_empty());

        // Afterwards, only normal characters remain: repeated backspaces just
        // erase them one by one and never resurrect the pasted text.
        while !view.input.is_empty() {
            view.backspace();
        }
        assert!(view.input.is_empty());
        assert!(view.pasted_parts.is_empty());
        assert_eq!(view.expand_pasted_text(&view.input), "");
    }

    #[test]
    fn ctrl_backspace_inside_pasted_placeholder_removes_it_whole() {
        let mut view = PromptView::new();
        view.handle_paste("a\nb\nc");
        assert_eq!(view.input, "[Pasted ~3 lines]");

        // Move the cursor into the MIDDLE of the placeholder (as if the user
        // arrowed left a few times), then Ctrl+Backspace: the entire
        // placeholder must go, not just the characters left of the cursor.
        let mid = view.input.find("Pasted").unwrap() + "Pasted".len();
        view.cursor_pos = mid;
        view.delete_word_before_cursor();
        assert!(view.input.is_empty());
        assert!(view.pasted_parts.is_empty());
        assert_eq!(view.cursor_pos, 0);
    }

    #[test]
    fn ctrl_backspace_keeps_word_before_a_pasted_placeholder_intact() {
        let mut view = PromptView::new();
        view.input = "alpha beta ".into();
        view.cursor_pos = view.input.len();
        view.handle_paste("x\ny\nz");
        assert_eq!(view.input, "alpha beta [Pasted ~3 lines]");

        // One Ctrl+Backspace deletes only the placeholder; the earlier words
        // stay untouched (each subsequent press deletes one word at a time).
        view.delete_word_before_cursor();
        assert_eq!(view.input, "alpha beta ");
        assert!(view.pasted_parts.is_empty());

        view.delete_word_before_cursor();
        assert_eq!(view.input, "alpha ");
    }
}
