use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tools::question::types::{AnswerItem, QuestionItem, QuestionType};
use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use super::super::super::component::cursor::{Cursor, CursorState};
use super::super::super::theme::Theme;
use crate::theme::rgba_color;

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters: writing them into buffer cells makes
        // ratatui's buffer diff panic ("control character passed to
        // cell_width without filtering").
        if ch.is_control() {
            continue;
        }
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

/// How many rows a PageUp/PageDown or mouse-wheel scroll step moves the
/// question-text region.
const SCROLL_STEP: usize = 3;

/// Virtual option appended by the TUI to every `SingleChoice` question so the
/// user can always give a free-text answer. Never sent by the model — the tool
/// description explicitly tells it NOT to invent its own custom/other entry.
pub const CUSTOM_RESPONSE_LABEL: &str = "Personalize your response";

/// Placeholder shown in the custom-answer input when it is empty.
const CUSTOM_PLACEHOLDER: &str = "Type your custom answer...";

/// Suffix rendered after the recommended `SingleChoice` option.
const RECOMMENDED_SUFFIX: &str = " (Recommended)";

const fn left_border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '┃',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '┃',
        right_t: ' ',
        cross: ' ',
    }
}

/// Precomputed styles used while rendering the review screen.
struct ReviewStyles {
    header: Style,
    label: Style,
    value: Style,
    unanswered: Style,
    purpose: Style,
}

/// Per-question state tracked by the dialog.
#[derive(Debug, Clone)]
struct QuestionState {
    /// For SingleChoice/YesNo: which option index is selected (None = nothing selected).
    /// For SingleChoice the virtual custom row lives at index
    /// `options.len()` (see [`QuestionDialog::custom_index`]).
    single_selection: Option<usize>,
    /// For `MultiChoice`: which option indices are checked
    multi_selection: Vec<usize>,
    /// For Text: the typed text input
    text_input: String,
    /// For Text: the cursor position (in chars) within `text_input`.
    cursor_pos: usize,
    /// For SingleChoice custom row: the typed custom answer.
    custom_text: String,
    /// Cursor position (in chars) within `custom_text`.
    custom_cursor: usize,
    /// Whether this question has been "answered" (user pressed Enter on it)
    answered: bool,
}

impl QuestionState {
    fn new() -> Self {
        Self {
            single_selection: None,
            multi_selection: Vec::new(),
            text_input: String::new(),
            cursor_pos: 0,
            custom_text: String::new(),
            custom_cursor: 0,
            answered: false,
        }
    }
}

/// Inline question dialog rendered inside the session chat (like `OpenCode`).
///
/// Receives questions from the `ask_questions` tool call via [`Self::show_questions`],
/// renders them inline, captures user input, and builds [`AnswerItem`]s via
/// [`Self::build_answers`].
pub struct QuestionDialog {
    pub visible: bool,
    /// Set to true when the user presses Enter on the confirm tab.
    /// The App checks this after `handle_key` and triggers answer submission.
    pub submitted: bool,
    /// The current questions being asked.
    questions: Vec<QuestionItem>,
    /// Per-question state (parallel to `questions`).
    state: Vec<QuestionState>,
    /// Which tab (question index) is active. If >= `questions.len()`, we're on the confirm screen.
    current_tab: usize,
    /// Which option row is highlighted (for keyboard navigation within a tab).
    selected_row: usize,
    /// Scroll offset (in rows) for the question-text region when it overflows
    /// the box height (e.g. a question longer than the viewport). Only the
    /// question text scrolls; options and the footer stay fixed.
    text_scroll: usize,
    /// Scroll offset (in flat display rows) for the options viewport. The box
    /// grows upward to fit the content first (`required_height`); this offset
    /// only kicks in as a last resort when the options alone exceed the
    /// available height. It auto-follows `selected_row` every frame, so the
    /// focused row — including the trailing custom row — is never stranded
    /// off-screen.
    opt_scroll: usize,
    /// Blinking cursor for the Text-answer input field. Its `terminal_focused`
    /// is synced by the App before each render, like every other cursor.
    pub cursor: Cursor,
}

/// One flat display row inside the options viewport. Every variant occupies
/// exactly one terminal row, so hit-testing is a plain index comparison.
#[derive(Debug, Clone)]
enum OptFlatRow {
    /// Wrapped line of selectable option `row`.
    Label { row: usize, text: String },
    /// The SingleChoice custom-answer input line below the custom row.
    CustomInput,
    /// Blank breathing row after option `row`'s block, so adjacent options
    /// read as separate items. Never trailing: the last block sits directly
    /// above the footer.
    Gap { row: usize },
}

impl QuestionDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            submitted: false,
            questions: Vec::new(),
            state: Vec::new(),
            current_tab: 0,
            selected_row: 0,
            text_scroll: 0,
            opt_scroll: 0,
            cursor: Cursor::new(),
        }
    }

    /// Reset the dialog with new questions from the harness.
    pub fn show_questions(&mut self, questions: Vec<QuestionItem>) {
        self.questions = questions;
        self.state = (0..self.questions.len())
            .map(|_| QuestionState::new())
            .collect();
        self.current_tab = 0;
        self.selected_row = 0;
        self.text_scroll = 0;
        self.opt_scroll = 0;
        self.submitted = false;
        self.visible = true;
    }

    /// Virtual custom-row index for a `SingleChoice` question: one past the
    /// last model-provided option. `None` for every other question type.
    fn custom_index(&self, tab: usize) -> Option<usize> {
        let q = self.questions.get(tab)?;
        if q.question_type != QuestionType::SingleChoice {
            return None;
        }
        Some(q.options.as_ref().map_or(0, Vec::len))
    }

    /// Whether the custom-answer input field should be drawn for `tab`.
    /// Always true for `SingleChoice`: the box starts pre-expanded with the
    /// field visible so users see the personalization affordance upfront —
    /// nobody must navigate first to discover it, and focusing the custom
    /// row never shifts the layout afterwards.
    fn should_show_custom_input(&self, tab: usize) -> bool {
        self.custom_index(tab).is_some()
    }

    /// Display text for a `SingleChoice` option, with the `(Recommended)`
    /// badge when it matches `q.recommended`. Robust to payloads that were
    /// not normalized (old sessions / tests): the badge is matched by value,
    /// not by position.
    fn display_option(q: &QuestionItem, opt: &str) -> String {
        if q.question_type == QuestionType::SingleChoice
            && q.recommended.as_deref() == Some(opt)
            && !opt.ends_with(RECOMMENDED_SUFFIX)
        {
            format!("{opt}{RECOMMENDED_SUFFIX}")
        } else {
            opt.to_string()
        }
    }

    /// Build the answer items from the current dialog state.
    #[must_use]
    pub fn build_answers(&self) -> Vec<AnswerItem> {
        self.questions
            .iter()
            .enumerate()
            .map(|(idx, q)| {
                let s = &self.state[idx];
                let (answer, selected) = match q.question_type {
                    QuestionType::Text => (Some(s.text_input.clone()), None),
                    QuestionType::SingleChoice => {
                        let custom = q.options.as_ref().map_or(0, Vec::len);
                        if s.single_selection == Some(custom) {
                            // Custom row committed: the free-text answer travels
                            // as the selection so the model sees what the user
                            // actually typed. Empty custom text counts as
                            // unanswered (consistent with other types).
                            if s.custom_text.is_empty() {
                                (None, None)
                            } else {
                                (None, Some(vec![s.custom_text.clone()]))
                            }
                        } else {
                            let sel = s
                                .single_selection
                                .and_then(|i| q.options.as_ref()?.get(i).cloned());
                            (None, sel.map(|s| vec![s]))
                        }
                    }
                    QuestionType::MultiChoice => {
                        let sel: Option<Vec<String>> = s
                            .multi_selection
                            .iter()
                            .filter_map(|&i| q.options.as_ref()?.get(i).cloned())
                            .collect::<Vec<_>>()
                            .into();
                        (
                            None,
                            if sel.as_ref().is_none_or(std::vec::Vec::is_empty) {
                                None
                            } else {
                                sel
                            },
                        )
                    }
                    QuestionType::YesNo => {
                        let ans = s
                            .single_selection
                            .map(|i| if i == 0 { "Yes".into() } else { "No".into() });
                        (ans, None)
                    }
                };
                AnswerItem {
                    id: q.id.clone(),
                    answer,
                    selected,
                }
            })
            .collect()
    }

    /// The total number of tabs: one per question + the confirm screen.
    const fn tab_count(&self) -> usize {
        if self.questions.is_empty() {
            1
        } else {
            self.questions.len() + 1
        }
    }

    /// Whether the current tab is the confirm screen.
    const fn is_confirm(&self) -> bool {
        self.current_tab >= self.questions.len()
    }

    /// Number of selectable rows on the current non-confirm tab.
    fn row_count(&self, tab: usize) -> usize {
        let Some(q) = self.questions.get(tab) else {
            return 0;
        };
        match q.question_type {
            QuestionType::Text => 1,  // just the text input field
            QuestionType::YesNo => 2, // Yes / No
            // SingleChoice always appends the virtual custom row so the user
            // can personalize even when the model did not offer that option.
            QuestionType::SingleChoice => q.options.as_ref().map_or(1, |o| o.len() + 1),
            QuestionType::MultiChoice => q.options.as_ref().map_or(0, std::vec::Vec::len),
        }
    }

    /// Insert `ch` into the current Text question's answer at the cursor and
    /// advance the cursor. Resets the blink (the user is actively typing).
    fn text_insert_char(&mut self, ch: char) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let pos = s.cursor_pos.min(s.text_input.chars().count());
            let byte_idx = s
                .text_input
                .char_indices()
                .nth(pos)
                .map_or(s.text_input.len(), |(i, _)| i);
            s.text_input.insert(byte_idx, ch);
            s.cursor_pos = pos + 1;
        }
        self.cursor.note_activity();
    }

    /// Delete the character before the cursor (Backspace).
    fn text_delete_before_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab)
            && s.cursor_pos > 0
        {
            let byte_idx = s
                .text_input
                .char_indices()
                .nth(s.cursor_pos - 1)
                .map_or(0, |(i, _)| i);
            s.text_input.remove(byte_idx);
            s.cursor_pos -= 1;
        }
        self.cursor.note_activity();
    }

    /// Delete the character under the cursor (Delete).
    fn text_delete_at_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab)
            && s.cursor_pos < s.text_input.chars().count()
        {
            let byte_idx = s
                .text_input
                .char_indices()
                .nth(s.cursor_pos)
                .map_or(s.text_input.len(), |(i, _)| i);
            s.text_input.remove(byte_idx);
        }
        self.cursor.note_activity();
    }

    /// Move the text cursor by `delta` chars, clamped to the input bounds.
    fn text_move_cursor(&mut self, delta: i32) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let len = s.text_input.chars().count() as i32;
            let new = (s.cursor_pos as i32 + delta).clamp(0, len);
            s.cursor_pos = new as usize;
        }
        self.cursor.note_activity();
    }

    /// Byte offset of the `char_pos`-th character in `text`.
    fn char_to_byte(text: &str, char_pos: usize) -> usize {
        text.char_indices()
            .nth(char_pos)
            .map_or(text.len(), |(i, _)| i)
    }

    /// Char index of the character containing byte offset `byte` in `text`.
    fn byte_to_char(text: &str, byte: usize) -> usize {
        text[..byte.min(text.len())].chars().count()
    }

    /// Move the cursor to the start of the previous word (Ctrl+Left), matching
    /// the chat prompt's `cursor_word_left`.
    fn text_cursor_word_left(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.text_input, s.cursor_pos);
            let new_byte = crate::util::word_ops::find_word_start(&s.text_input, byte);
            if new_byte < byte {
                s.cursor_pos = Self::byte_to_char(&s.text_input, new_byte);
            }
        }
        self.cursor.note_activity();
    }

    /// Move the cursor to the start of the next word (Ctrl+Right), matching
    /// the chat prompt's `cursor_word_right`.
    fn text_cursor_word_right(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.text_input, s.cursor_pos);
            let new_byte = crate::util::word_ops::find_word_end(&s.text_input, byte);
            if new_byte > byte {
                s.cursor_pos = Self::byte_to_char(&s.text_input, new_byte);
            }
        }
        self.cursor.note_activity();
    }

    /// Delete the word (or run of whitespace then word) immediately before the
    /// cursor (Ctrl+Backspace / Ctrl+W), matching the chat prompt's
    /// `delete_word_before_cursor`.
    fn text_delete_word_before_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.text_input, s.cursor_pos);
            let start = crate::util::word_ops::find_word_start(&s.text_input, byte);
            if start < byte {
                s.text_input.drain(start..byte);
                s.cursor_pos = Self::byte_to_char(&s.text_input, start);
            }
        }
        self.cursor.note_activity();
    }

    /// Whether the custom-answer input for the current tab is focused: a
    /// `SingleChoice` tab with the virtual custom row highlighted.
    fn is_custom_focused(&self) -> bool {
        if self.is_confirm() {
            return false;
        }
        let Some(q) = self.questions.get(self.current_tab) else {
            return false;
        };
        if q.question_type != QuestionType::SingleChoice {
            return false;
        }
        self.custom_index(self.current_tab) == Some(self.selected_row)
    }

    /// Insert `ch` into the SingleChoice custom answer at its cursor.
    fn custom_insert_char(&mut self, ch: char) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let pos = s.custom_cursor.min(s.custom_text.chars().count());
            let byte_idx = Self::char_to_byte(&s.custom_text, pos);
            s.custom_text.insert(byte_idx, ch);
            s.custom_cursor = pos + 1;
        }
        self.cursor.note_activity();
    }

    /// Delete the character before the custom cursor (Backspace).
    fn custom_delete_before_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab)
            && s.custom_cursor > 0
        {
            let byte_idx = Self::char_to_byte(&s.custom_text, s.custom_cursor - 1);
            s.custom_text.remove(byte_idx);
            s.custom_cursor -= 1;
        }
        self.cursor.note_activity();
    }

    /// Delete the character under the custom cursor (Delete).
    fn custom_delete_at_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab)
            && s.custom_cursor < s.custom_text.chars().count()
        {
            let byte_idx = Self::char_to_byte(&s.custom_text, s.custom_cursor);
            s.custom_text.remove(byte_idx);
        }
        self.cursor.note_activity();
    }

    /// Move the custom cursor by `delta` chars, clamped to the input bounds.
    fn custom_move_cursor(&mut self, delta: i32) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let len = s.custom_text.chars().count() as i32;
            let new = (s.custom_cursor as i32 + delta).clamp(0, len);
            s.custom_cursor = new as usize;
        }
        self.cursor.note_activity();
    }

    /// Move the custom cursor to the start of the previous word (Ctrl+Left).
    fn custom_cursor_word_left(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.custom_text, s.custom_cursor);
            let new_byte = crate::util::word_ops::find_word_start(&s.custom_text, byte);
            if new_byte < byte {
                s.custom_cursor = Self::byte_to_char(&s.custom_text, new_byte);
            }
        }
        self.cursor.note_activity();
    }

    /// Move the custom cursor to the start of the next word (Ctrl+Right).
    fn custom_cursor_word_right(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.custom_text, s.custom_cursor);
            let new_byte = crate::util::word_ops::find_word_end(&s.custom_text, byte);
            if new_byte > byte {
                s.custom_cursor = Self::byte_to_char(&s.custom_text, new_byte);
            }
        }
        self.cursor.note_activity();
    }

    /// Delete the word before the custom cursor (Ctrl+Backspace / Ctrl+W).
    fn custom_delete_word_before_cursor(&mut self) {
        if let Some(s) = self.state.get_mut(self.current_tab) {
            let byte = Self::char_to_byte(&s.custom_text, s.custom_cursor);
            let start = crate::util::word_ops::find_word_start(&s.custom_text, byte);
            if start < byte {
                s.custom_text.drain(start..byte);
                s.custom_cursor = Self::byte_to_char(&s.custom_text, start);
            }
        }
        self.cursor.note_activity();
    }

    /// Paste text into the current Text question's answer at the cursor
    /// (bracketed paste from the terminal, like the chat prompt). Also serves
    /// the SingleChoice custom-answer input when its virtual row is focused.
    pub fn handle_paste(&mut self, text: &str) {
        if !self.visible {
            return;
        }
        let is_text = self
            .questions
            .get(self.current_tab)
            .is_some_and(|q| q.question_type == QuestionType::Text);
        if is_text {
            if let Some(s) = self.state.get_mut(self.current_tab) {
                // The field is single-line: strip newlines so the pasted text
                // stays aligned with the horizontal-scroll rendering (matching
                // the ApiKeyInput dialog's paste handling).
                let cleaned: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                let byte = Self::char_to_byte(&s.text_input, s.cursor_pos);
                s.text_input.insert_str(byte, &cleaned);
                s.cursor_pos += cleaned.chars().count();
            }
            self.cursor.note_activity();
            return;
        }
        if self.is_custom_focused() {
            if let Some(s) = self.state.get_mut(self.current_tab) {
                let cleaned: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                let byte = Self::char_to_byte(&s.custom_text, s.custom_cursor);
                s.custom_text.insert_str(byte, &cleaned);
                s.custom_cursor += cleaned.chars().count();
            }
            self.cursor.note_activity();
        }
    }

    /// Handle a single key code with no modifiers. The App passes the full
    /// [`KeyEvent`] (code + modifiers) to [`Self::handle_key_event`]; this
    /// variant is used by tests and internal single-key dispatches.
    pub fn handle_key(&mut self, key: KeyCode) -> bool {
        self.handle_key_event(KeyEvent::new(key, KeyModifiers::NONE))
    }

    /// Handle key events (code + modifiers). Returns true if the key was
    /// consumed. Mirrors the chat prompt's editing shortcuts: Ctrl+Left/Right
    /// move by word, Ctrl+Backspace and Ctrl+W delete the word before the
    /// cursor.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }

        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let tab_count = self.tab_count();
        let is_confirm = self.is_confirm();

        // Text-answer questions are always an active input field: typing
        // inserts at the cursor, Left/Right/Home/End move it, Enter commits
        // and moves to the next tab, and Tab keeps navigating tabs.
        if !is_confirm
            && let Some(q) = self.questions.get(self.current_tab)
            && q.question_type == QuestionType::Text
        {
            match code {
                // Ctrl+W deletes the word before the cursor (universal
                // terminal shortcut, same as the chat prompt). Other
                // Ctrl+char combos (Ctrl+C, Ctrl+A, ...) are consumed but
                // ignored so they never leak bare letters into the answer.
                KeyCode::Char(ch) if !ctrl => {
                    self.text_insert_char(ch);
                    return true;
                }
                KeyCode::Char('w') if ctrl => {
                    self.text_delete_word_before_cursor();
                    return true;
                }
                KeyCode::Char(_) if ctrl => {
                    return true;
                }
                KeyCode::Backspace => {
                    if ctrl {
                        self.text_delete_word_before_cursor();
                    } else {
                        self.text_delete_before_cursor();
                    }
                    return true;
                }
                KeyCode::Delete => {
                    self.text_delete_at_cursor();
                    return true;
                }
                KeyCode::Left => {
                    if ctrl {
                        self.text_cursor_word_left();
                    } else {
                        self.text_move_cursor(-1);
                    }
                    return true;
                }
                KeyCode::Right => {
                    if ctrl {
                        self.text_cursor_word_right();
                    } else {
                        self.text_move_cursor(1);
                    }
                    return true;
                }
                KeyCode::Home => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.cursor_pos = 0;
                    }
                    self.cursor.note_activity();
                    return true;
                }
                KeyCode::End => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.cursor_pos = s.text_input.chars().count();
                    }
                    self.cursor.note_activity();
                    return true;
                }
                // Tab keeps navigating tabs even while editing.
                KeyCode::Tab => {
                    if tab_count > 1 {
                        self.current_tab = (self.current_tab + 1) % tab_count;
                        self.selected_row = 0;
                        self.text_scroll = 0;
                    }
                    return true;
                }
                KeyCode::Enter => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.answered = true;
                    }
                    // Move to next tab
                    if tab_count > 1 {
                        self.current_tab = (self.current_tab + 1) % tab_count;
                        self.selected_row = 0;
                        self.text_scroll = 0;
                    }
                    return true;
                }
                _ => {}
            }
        }

        // SingleChoice custom row is an inline free-text field: printable keys
        // type into it, Left/Right/Home/End move its cursor, Enter commits the
        // custom answer and advances. Up/Down/Tab/Esc intentionally fall
        // through so the user can still leave the row (arrows), switch tabs
        // (Tab) or dismiss (Esc) while focused on it.
        if !is_confirm && self.is_custom_focused() {
            match code {
                KeyCode::Char(ch) if !ctrl => {
                    self.custom_insert_char(ch);
                    return true;
                }
                KeyCode::Char('w') if ctrl => {
                    self.custom_delete_word_before_cursor();
                    return true;
                }
                KeyCode::Char(_) if ctrl => {
                    return true;
                }
                KeyCode::Backspace => {
                    if ctrl {
                        self.custom_delete_word_before_cursor();
                    } else {
                        self.custom_delete_before_cursor();
                    }
                    return true;
                }
                KeyCode::Delete => {
                    self.custom_delete_at_cursor();
                    return true;
                }
                KeyCode::Left => {
                    if ctrl {
                        self.custom_cursor_word_left();
                    } else {
                        self.custom_move_cursor(-1);
                    }
                    return true;
                }
                KeyCode::Right => {
                    if ctrl {
                        self.custom_cursor_word_right();
                    } else {
                        self.custom_move_cursor(1);
                    }
                    return true;
                }
                KeyCode::Home => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.custom_cursor = 0;
                    }
                    self.cursor.note_activity();
                    return true;
                }
                KeyCode::End => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.custom_cursor = s.custom_text.chars().count();
                    }
                    self.cursor.note_activity();
                    return true;
                }
                KeyCode::Tab => {
                    if tab_count > 1 {
                        self.current_tab = (self.current_tab + 1) % tab_count;
                        self.selected_row = 0;
                        self.text_scroll = 0;
                    }
                    return true;
                }
                KeyCode::Enter => {
                    if let Some(custom) = self.custom_index(self.current_tab)
                        && let Some(s) = self.state.get_mut(self.current_tab)
                    {
                        s.single_selection = Some(custom);
                        s.answered = true;
                    }
                    if tab_count > 1 {
                        self.current_tab = (self.current_tab + 1) % tab_count;
                        self.selected_row = 0;
                        self.text_scroll = 0;
                    }
                    return true;
                }
                _ => {}
            }
        }

        // Tab navigation (Left/Right/h/l/Tab move between tabs on option tabs).
        // While the custom input is focused, Left/Right/Tab/h/l are consumed
        // by the custom-editing block above, so they never reach here.
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                if tab_count > 1 {
                    self.current_tab = if self.current_tab == 0 {
                        tab_count - 1
                    } else {
                        self.current_tab - 1
                    };
                    self.selected_row = 0;
                    self.text_scroll = 0;
                }
                return true;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                if tab_count > 1 {
                    self.current_tab = (self.current_tab + 1) % tab_count;
                    self.selected_row = 0;
                    self.text_scroll = 0;
                }
                return true;
            }
            _ => {}
        }

        // Options navigation (non-confirm tabs)
        if is_confirm {
            // Confirm tab
            match code {
                KeyCode::Enter => {
                    self.submitted = true;
                    return true;
                }
                KeyCode::Esc => {
                    self.visible = false;
                    return true;
                }
                KeyCode::PageUp => {
                    self.text_scroll = self.text_scroll.saturating_sub(SCROLL_STEP);
                    return true;
                }
                KeyCode::PageDown => {
                    self.text_scroll = self.text_scroll.saturating_add(SCROLL_STEP);
                    return true;
                }
                _ => {}
            }
        } else {
            // Esc dismisses the dialog (like OpenCode)
            if code == KeyCode::Esc {
                self.visible = false;
                return true;
            }

            let count = self.row_count(self.current_tab);

            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if count > 0 {
                        self.selected_row = if self.selected_row == 0 {
                            count - 1
                        } else {
                            self.selected_row - 1
                        };
                    }
                    return true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if count > 0 {
                        self.selected_row = (self.selected_row + 1) % count;
                    }
                    return true;
                }
                // Scroll the question text when it overflows the box.
                KeyCode::PageUp => {
                    self.text_scroll = self.text_scroll.saturating_sub(SCROLL_STEP);
                    return true;
                }
                KeyCode::PageDown => {
                    self.text_scroll = self.text_scroll.saturating_add(SCROLL_STEP);
                    return true;
                }
                // Number keys (1-9) to select and activate options directly (like OpenCode).
                // On Text-answer tabs these are typed as characters by the edit
                // block above, so they never reach here.
                KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                    let num = (c as usize) - ('1' as usize);
                    if num < count {
                        self.selected_row = num;
                        // Auto-select the option (like OpenCode's moveTo + selectOption)
                        return self.handle_key(KeyCode::Enter);
                    }
                    // Always consume number keys to prevent leaks to the prompt
                    return true;
                }
                KeyCode::Enter => {
                    if let Some(q) = self.questions.get(self.current_tab) {
                        match q.question_type {
                            // Text-answer tabs are handled by the edit block above.
                            QuestionType::Text => return true,
                            QuestionType::SingleChoice | QuestionType::YesNo => {
                                if let Some(s) = self.state.get_mut(self.current_tab) {
                                    s.single_selection = Some(self.selected_row);
                                    s.answered = true;
                                }
                                // Move to next tab
                                if tab_count > 1 {
                                    self.current_tab = (self.current_tab + 1) % tab_count;
                                    self.selected_row = 0;
                                    self.text_scroll = 0;
                                }
                                return true;
                            }
                            QuestionType::MultiChoice => {
                                if let Some(s) = self.state.get_mut(self.current_tab) {
                                    let idx = self.selected_row;
                                    if let Some(pos) =
                                        s.multi_selection.iter().position(|&i| i == idx)
                                    {
                                        s.multi_selection.remove(pos);
                                    } else {
                                        s.multi_selection.push(idx);
                                    }
                                    // Stay on this tab to allow more selections
                                }
                                return true;
                            }
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }

        false
    }

    /// Handle a mouse click on the question dialog.
    /// `area` is the area passed to `render()`.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.visible {
            return false;
        }

        let x = mouse.x;
        let y_click = mouse.y;

        // Check if click is within our bounds
        let height = self.required_height(area.width).min(area.height);
        if x < area.x || x >= area.x + area.width || y_click < area.y || y_click >= area.y + height
        {
            return false;
        }

        let tab_count = self.tab_count();
        let is_confirm = self.is_confirm();

        // --- Hit-test the footer action labels ---
        // `footer_y` must match `render`, where the hints are drawn at
        // `inner_area.bottom() - 2`.
        let inner_x = area.x + 3;
        let inner_w = area.width.saturating_sub(5);
        let footer_y = area.y + height.saturating_sub(2);

        // "esc" footer label
        let esc_label = "esc";
        if y_click == footer_y {
            // Determine the x position of the "esc" label in the footer
            let mut fx = inner_x;
            if tab_count > 1 {
                fx += 4 + 4; // "⇆" + "tab"
            }
            fx += 5; // "enter"
            fx += if is_confirm {
                "submit".len() as u16
            } else {
                "select".len() as u16
            };
            fx += 2;
            // Now fx points to "esc"
            let esc_x = fx;
            if x >= esc_x && x < esc_x + esc_label.len() as u16 {
                self.visible = false;
                return true;
            }
        }

        // --- Hit-test the tab bar ---
        if tab_count > 1 {
            let mut tab_x = inner_x;
            let tab_y = area.y + 1;
            if y_click == tab_y {
                for (i, q) in self.questions.iter().enumerate() {
                    let label = format!(" {} ", q.id);
                    let label_len = label.len() as u16;
                    if x >= tab_x && x < tab_x + label_len {
                        self.current_tab = i;
                        self.selected_row = 0;
                        self.text_scroll = 0;
                        return true;
                    }
                    tab_x += label_len + 1;
                }
                // Confirm tab
                let confirm_label = " Confirm ";
                if x >= tab_x && x < tab_x + confirm_label.len() as u16 {
                    self.current_tab = self.questions.len();
                    self.selected_row = 0;
                    self.text_scroll = 0;
                    return true;
                }
                return true;
            }
        }

        // --- Hit-test options ---
        if is_confirm {
            // Confirm tab: clicking anywhere on the confirm tab can submit
            if y_click > area.y && y_click < footer_y {
                self.submitted = true;
                return true;
            }
        } else {
            // Same layout as `render`: top pad (1), plus the tab-bar row and
            // its separator (+2) only when several questions share the dialog
            // (`questions.len()`, not the `+1` confirm count used below for
            // navigation).
            let start_y = area.y + 1 + 2 * u16::from(self.questions.len() > 1);
            let opt_rows = self.option_row_count(self.current_tab, inner_w);
            // The question-text region occupies a fixed number of rows; the
            // options start right below it (plus a 1-row gap). Same layout as
            // `render`.
            let text_h = footer_y
                .saturating_sub(start_y)
                .saturating_sub(opt_rows)
                .saturating_sub(1);
            let option_y = start_y + text_h + 1;

            if let Some(q) = self.questions.get(self.current_tab).cloned() {
                let tab = self.current_tab;
                // Same viewport as `render_options`: visible window over the
                // flat rows starting at `opt_scroll`.
                let capacity = footer_y.saturating_sub(option_y) as usize;
                self.ensure_selected_visible(tab, inner_w, capacity);
                let flat = self.flat_option_rows(tab, inner_w);
                let custom_idx = self.custom_index(tab);
                let base = self.opt_scroll;
                for (ai, frow) in flat.iter().enumerate().skip(base).take(capacity) {
                    let oy = option_y + (ai - base) as u16;
                    if y_click != oy {
                        continue;
                    }
                    match frow {
                        OptFlatRow::CustomInput => {
                            // Clicking the input focuses the custom row and
                            // places the cursor, mirroring Text input clicks.
                            if let Some(custom) = custom_idx {
                                self.selected_row = custom;
                            }
                            let text_x = inner_x + 2;
                            if let Some(s) = self.state.get_mut(tab) {
                                let len = s.custom_text.chars().count();
                                let field_w = inner_w.saturating_sub(2) as usize;
                                let h_scroll = if s.custom_cursor >= field_w && len > field_w {
                                    s.custom_cursor - field_w + 1
                                } else {
                                    0
                                };
                                let clicked = (i64::from(x).saturating_sub(i64::from(text_x)))
                                    .max(0) as usize;
                                s.custom_cursor = (h_scroll + clicked).min(len);
                            }
                            self.cursor.note_activity();
                            return true;
                        }
                        // A gap row acts as its option above (generous click
                        // target): clicking breathing space selects just like
                        // clicking the option itself.
                        OptFlatRow::Label { row, .. } | OptFlatRow::Gap { row } => {
                            let row = *row;
                            self.selected_row = row;
                            match q.question_type {
                                QuestionType::Text => {
                                    // Click the input field: place the cursor at the
                                    // clicked character position.
                                    let text_x = inner_x + 2; // after the "  " prefix
                                    if let Some(s) = self.state.get_mut(tab) {
                                        let len = s.text_input.chars().count();
                                        let field_w = inner_w.saturating_sub(2) as usize;
                                        // Replicate `render`'s h_scroll so clicks land
                                        // on the characters currently visible.
                                        let h_scroll = if s.cursor_pos >= field_w && len > field_w {
                                            s.cursor_pos - field_w + 1
                                        } else {
                                            0
                                        };
                                        let clicked =
                                            (i64::from(x).saturating_sub(i64::from(text_x))).max(0)
                                                as usize;
                                        s.cursor_pos = (h_scroll + clicked).min(len);
                                    }
                                    self.cursor.note_activity();
                                    return true;
                                }
                                QuestionType::SingleChoice => {
                                    let is_custom = custom_idx == Some(row);
                                    if is_custom {
                                        // Focus the custom input; don't advance —
                                        // the user still has to type + Enter.
                                        self.cursor.note_activity();
                                        return true;
                                    }
                                    if let Some(s) = self.state.get_mut(tab) {
                                        s.single_selection = Some(row);
                                        s.answered = true;
                                    }
                                    // Move to next tab
                                    if tab_count > 1 {
                                        self.current_tab = (self.current_tab + 1) % tab_count;
                                        self.selected_row = 0;
                                        self.text_scroll = 0;
                                    }
                                    return true;
                                }
                                QuestionType::YesNo => {
                                    if let Some(s) = self.state.get_mut(tab) {
                                        s.single_selection = Some(row);
                                        s.answered = true;
                                    }
                                    // Move to next tab
                                    if tab_count > 1 {
                                        self.current_tab = (self.current_tab + 1) % tab_count;
                                        self.selected_row = 0;
                                        self.text_scroll = 0;
                                    }
                                    return true;
                                }
                                QuestionType::MultiChoice => {
                                    if let Some(s) = self.state.get_mut(tab) {
                                        if let Some(pos) =
                                            s.multi_selection.iter().position(|&i| i == row)
                                        {
                                            s.multi_selection.remove(pos);
                                        } else {
                                            s.multi_selection.push(row);
                                        }
                                    }
                                    return true;
                                }
                            }
                        }
                    }
                }
            }

            // Check if click is on the "enter select" area
            if y_click == footer_y {
                // The "enter" label is at fx position (same computation)
                let mut fx = inner_x;
                if tab_count > 1 {
                    fx += 4 + 4;
                }
                let enter_x = fx;
                let enter_label = "enter";
                if x >= enter_x && x < enter_x + enter_label.len() as u16 {
                    // "select" action
                    return self.handle_key(KeyCode::Enter);
                }
            }
        }

        true
    }

    /// Calculate the required height for the dialog.
    /// Public so that the App can allocate space before rendering.
    ///
    /// Growth policy: the box grows to fit its content for as long as the
    /// screen allows — the only limit is the App-level responsive clamp
    /// (`max_dialog_h`, i.e. the free space above the footer). Scrolling
    /// (question text first, then the options viewport with focus-follow) only
    /// engages past that budget, as a last resort. Deliberately there is NO
    /// absolute content cap (unlike the prompt box, whose typing area caps at
    /// `MAX_PROMPT_LINES`): an absolute cap would fold questionnaire items
    /// away while free screen space is still available.
    pub fn required_height(&self, max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }

        // Vertical chrome: top pad (1) + question/options gap (1) + bottom
        // pad (1), plus the tab-bar row and its separator (1 + 1) only when
        // several questions share the dialog. The footer hints row is counted
        // separately.
        let padding_vertical = 3u16;
        // Tab-bar row + separator row, only when several questions share the
        // dialog (mirrors `render`, where a lone question gets a single top
        // pad row and no separator).
        let tab_chrome = 2 * u16::from(self.questions.len() > 1);
        let footer = 1u16;
        // pad(3) + 2, matching `inner_w` used by `render`.
        let inner_w = max_width.saturating_sub(5);

        if self.is_confirm() {
            // "Review your answers:" + one (possibly wrapped) row per question
            // + wrapped purpose hints.
            let mut rows = 1u16;
            for (i, q) in self.questions.iter().enumerate() {
                let summary = self.question_summary(i);
                let label = format!(" {}: ", q.id);
                let label_w = label.chars().count() as u16;
                let val_lines =
                    Self::wrap_text(&summary, inner_w.saturating_sub(label_w)).len() as u16;
                rows += val_lines.max(1);
                if let Some(purpose) = &q.purpose {
                    rows += Self::wrap_decorated(purpose, "  └ ", "", inner_w).len() as u16;
                }
            }
            padding_vertical + rows + tab_chrome + footer
        } else if let Some(q) = self.questions.get(self.current_tab) {
            let q_lines = Self::wrap_text(&q.question, inner_w).len() as u16;
            let p_lines = q.purpose.as_ref().map_or(0, |p| {
                Self::wrap_decorated(p, "  (", ")", inner_w).len() as u16
            });
            let opt_rows = self.option_row_count(self.current_tab, inner_w);
            padding_vertical + q_lines.max(1) + p_lines + opt_rows + tab_chrome + footer
        } else {
            padding_vertical + footer
        }
    }
    /// Scroll the question-text region down by one step (used by the mouse wheel).
    pub fn scroll_down(&mut self) {
        if !self.visible {
            return;
        }
        self.text_scroll = self.text_scroll.saturating_add(SCROLL_STEP);
    }

    /// Scroll the question-text region up by one step (used by the mouse wheel).
    pub fn scroll_up(&mut self) {
        if !self.visible {
            return;
        }
        self.text_scroll = self.text_scroll.saturating_sub(SCROLL_STEP);
    }

    /// Move keyboard focus by `delta` selectable rows, clamped at the ends.
    /// Unlike Up/Down (which wrap), the wheel never wraps — it just stops at
    /// the first/last option. Focus-only: nothing is committed and the tab
    /// never changes; the viewport auto-follows on the next render.
    fn move_focus(&mut self, delta: i32) {
        let count = self.row_count(self.current_tab) as i32;
        if count <= 0 {
            return;
        }
        let next = (self.selected_row as i32 + delta).clamp(0, count - 1);
        self.selected_row = next as usize;
    }

    /// Handle a mouse-wheel notch over the dialog (`area` is the area passed
    /// to `render()`). The wheel is routed by region so every folded content
    /// stays reachable by mouse: over the options viewport it walks focus
    /// through the options (the viewport auto-follows); anywhere else it
    /// scrolls the question text (or the review screen), as before.
    /// Returns true when the dialog is visible (the notch is consumed).
    pub fn handle_wheel(&mut self, y: u16, area: Rect, down: bool) -> bool {
        if !self.visible {
            return false;
        }
        if self.is_confirm() {
            // Review screen scrolls through `text_scroll`.
            if down {
                self.scroll_down();
            } else {
                self.scroll_up();
            }
            return true;
        }
        // Same layout as `render`/`handle_mouse`: top pad (1), plus the
        // tab-bar row and its separator (+2) only for several questions.
        let inner_w = area.width.saturating_sub(5);
        let height = self.required_height(area.width).min(area.height);
        let footer_y = area.y + height.saturating_sub(2);
        let start_y = area.y + 1 + 2 * u16::from(self.questions.len() > 1);
        let opt_rows = self.option_row_count(self.current_tab, inner_w);
        let text_h = footer_y
            .saturating_sub(start_y)
            .saturating_sub(opt_rows)
            .saturating_sub(1);
        let option_y = start_y + text_h + 1;

        let is_options_tab = self.questions.get(self.current_tab).is_some_and(|q| {
            matches!(
                q.question_type,
                QuestionType::SingleChoice | QuestionType::YesNo | QuestionType::MultiChoice
            )
        });
        if is_options_tab && y >= option_y && y < footer_y {
            self.move_focus(if down { 1 } else { -1 });
        } else if down {
            self.scroll_down();
        } else {
            self.scroll_up();
        }
        true
    }

    /// Wrap `text` into display lines of at most `width` characters.
    ///
    /// - Runs of whitespace collapse into a single space.
    /// - Words longer than `width` are hard-split across lines.
    /// - Literal `\n` force a line break; blank paragraphs are preserved.
    /// - Control characters are dropped: writing them into buffer cells makes
    ///   ratatui's buffer diff panic ("control character passed to
    ///   cell_width without filtering").
    fn wrap_text(text: &str, width: u16) -> Vec<String> {
        let width = usize::from(width).max(1);
        let mut lines: Vec<String> = Vec::new();
        let mut line: String = String::new();

        for (pi, paragraph) in text.split('\n').enumerate() {
            // Each paragraph boundary closes the current line. Empty
            // paragraphs already produced their blank line, so a boundary
            // after one must not push a second blank.
            if pi > 0 && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }

            let words: Vec<String> = paragraph
                .split_whitespace()
                .map(|w| w.chars().filter(|c| !c.is_control()).collect())
                .filter(|w: &String| !w.is_empty())
                .collect();

            if words.is_empty() {
                // Blank paragraph (empty text, or a double newline).
                lines.push(String::new());
                continue;
            }

            for word in words {
                // Hard-split words that are wider than the box.
                let mut rest = word.as_str();
                loop {
                    let piece: String = rest.chars().take(width).collect();
                    if piece.is_empty() {
                        break;
                    }
                    rest = &rest[piece.len()..];
                    let fits = line.is_empty()
                        || line.chars().count() + 1 + piece.chars().count() <= width;
                    if fits {
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(&piece);
                        if !rest.is_empty() {
                            // The word continues on the next line.
                            lines.push(std::mem::take(&mut line));
                        }
                    } else {
                        lines.push(std::mem::take(&mut line));
                        line = piece;
                        if !rest.is_empty() {
                            lines.push(std::mem::take(&mut line));
                        }
                    }
                }
            }
        }
        lines.push(line);

        // Trim trailing blank lines (keep at least one line).
        while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    /// Wrap a decorated hint: `prefix` (e.g. `"  ("`) goes on the first line,
    /// continuation lines are indented to align under it, and `suffix` (e.g.
    /// `")"`) is appended to the final line.
    fn wrap_decorated(text: &str, prefix: &str, suffix: &str, width: u16) -> Vec<String> {
        let prefix_w = prefix.chars().count() as u16;
        let inner = width.saturating_sub(prefix_w);
        let body = Self::wrap_text(text, inner);
        let body_len = body.len();
        let mut out: Vec<String> = Vec::with_capacity(body_len);
        for (i, l) in body.into_iter().enumerate() {
            if i == 0 {
                if body_len == 1 {
                    out.push(format!("{prefix}{l}{suffix}"));
                } else {
                    out.push(format!("{prefix}{l}"));
                }
            } else {
                out.push(format!("{:p$}{l}", "", p = prefix_w as usize));
            }
        }
        if body_len > 1
            && let Some(last) = out.last_mut()
        {
            last.push_str(suffix);
        }
        out
    }

    /// Wrapped display lines for each option of the given tab. The option text
    /// width excludes the 3-column indicator (`◉ ` / `☐ ` etc.). For
    /// `SingleChoice` the last entry is always the virtual
    /// [`CUSTOM_RESPONSE_LABEL`] row appended by the TUI.
    fn option_wrapped_lines(&self, tab: usize, inner_w: u16) -> Vec<Vec<String>> {
        let Some(q) = self.questions.get(tab) else {
            return Vec::new();
        };
        let opt_w = inner_w.saturating_sub(3);
        match q.question_type {
            // The text input field occupies a single row; the placeholder line
            // keeps `option_row_count`/hit-testing consistent with the other
            // question types.
            QuestionType::Text => vec![vec![String::new()]],
            QuestionType::YesNo => vec!["Yes", "No"]
                .into_iter()
                .map(|o| Self::wrap_text(o, opt_w))
                .collect(),
            QuestionType::MultiChoice => q
                .options
                .as_deref()
                .map(|o| o.iter().map(String::as_str).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .map(|o| Self::wrap_text(o, opt_w))
                .collect(),
            QuestionType::SingleChoice => {
                let mut out: Vec<Vec<String>> = Vec::new();
                if let Some(opts) = q.options.as_deref() {
                    for opt in opts {
                        out.push(Self::wrap_text(&Self::display_option(q, opt), opt_w));
                    }
                }
                out.push(Self::wrap_text(CUSTOM_RESPONSE_LABEL, opt_w));
                out
            }
        }
    }

    /// Total display rows occupied by the given tab's options: wrapped lines,
    /// the SingleChoice custom-answer input line, and the blank gap row after
    /// each option block but the last. Defined via [`Self::flat_option_rows`]
    /// so height math can never drift from what is rendered and hit-tested.
    fn option_row_count(&self, tab: usize, inner_w: u16) -> u16 {
        self.flat_option_rows(tab, inner_w).len() as u16
    }

    /// Flat display rows of the options viewport for `tab`, in order: every
    /// wrapped option line, the custom input line below the custom row, and a
    /// blank gap row after each option block — between blocks so the eye can
    /// tell where one option ends and the next begins, plus one trailing the
    /// custom input so it never glues to the footer. Each entry is exactly
    /// one terminal row.
    fn flat_option_rows(&self, tab: usize, inner_w: u16) -> Vec<OptFlatRow> {
        let wrapped = self.option_wrapped_lines(tab, inner_w);
        let last = wrapped.len().saturating_sub(1);
        let mut out = Vec::new();
        for (row, lines) in wrapped.iter().enumerate() {
            for line in lines {
                out.push(OptFlatRow::Label {
                    row,
                    text: line.clone(),
                });
            }
            let is_custom_input =
                self.custom_index(tab) == Some(row) && self.should_show_custom_input(tab);
            if is_custom_input {
                out.push(OptFlatRow::CustomInput);
            }
            if row < last || is_custom_input {
                out.push(OptFlatRow::Gap { row });
            }
        }
        out
    }

    /// Absolute flat-row range `[start, end)` of selectable option `row`
    /// (including its custom input line when present). `None` when `row` is
    /// out of range (e.g. an empty `MultiChoice` payload rejected later).
    fn option_flat_range(&self, tab: usize, inner_w: u16, row: usize) -> Option<(usize, usize)> {
        let flat = self.flat_option_rows(tab, inner_w);
        let start = flat
            .iter()
            .position(|r| matches!(r, OptFlatRow::Label { row: r, .. } if *r == row))?;
        // The block runs through the option's wrapped lines plus, for the
        // custom row, its trailing input line (by construction it follows
        // immediately).
        let mut end = start;
        while end < flat.len() {
            match &flat[end] {
                OptFlatRow::Label { row: r, .. } if *r == row => end += 1,
                OptFlatRow::CustomInput => {
                    // Only reachable right after the custom row's own lines.
                    end += 1;
                    break;
                }
                _ => break,
            }
        }
        Some((start, end))
    }

    /// Clamp `opt_scroll` so the `selected_row` block is visible inside a
    /// viewport of `capacity` flat rows. Shifts minimally; when the block is
    /// taller than the viewport its start is shown. With room to spare the
    /// offset returns to 0 (no scroll chrome in the common case).
    fn ensure_selected_visible(&mut self, tab: usize, inner_w: u16, capacity: usize) {
        let total = self.flat_option_rows(tab, inner_w).len();
        if capacity == 0 || total <= capacity {
            self.opt_scroll = 0;
            return;
        }
        let max_scroll = total.saturating_sub(capacity);
        let Some((s0, s1)) = self.option_flat_range(tab, inner_w, self.selected_row) else {
            self.opt_scroll = self.opt_scroll.min(max_scroll);
            return;
        };
        if s0 < self.opt_scroll {
            self.opt_scroll = s0;
        } else if s1 > self.opt_scroll.saturating_add(capacity) {
            if s1.saturating_sub(s0) >= capacity {
                self.opt_scroll = s0;
            } else {
                self.opt_scroll = s1.saturating_sub(capacity);
            }
        }
        self.opt_scroll = self.opt_scroll.min(max_scroll);
    }

    /// Render the question prompt inline inside the given area.
    /// `now` drives the input cursor's blink state (passed by the App).
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, now: SystemTime) {
        if !self.visible {
            return;
        }

        let is_confirm = self.is_confirm();
        let tab_count = self.questions.len();
        let height = self.required_height(area.width).min(area.height);

        let inner_area = Rect::new(area.x, area.y, area.width, height);
        // Footer row: drawn last, but its y must be known before the content
        // is laid out so content never overlaps it.
        let footer_y = inner_area.bottom().saturating_sub(2);

        // Left border + background (matches OpenCode QuestionPrompt style)
        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.accent.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(left_border_chars());
        border_box.render_self(buf, inner_area);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let mut y_pos = area.y + 1;

        // --- Tab bar ---
        if tab_count > 1 {
            let mut tab_x = inner_x;
            for (i, q) in self.questions.iter().enumerate() {
                let is_active = i == self.current_tab;
                let tab_background = if is_active {
                    rgba_color(theme.accent)
                } else {
                    rgba_color(theme.background_panel)
                };
                let tab_foreground = if is_active {
                    let (red, green, blue, _) = theme.accent.to_ints();
                    let lum =
                        0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue);
                    if lum > 128.0 {
                        RGBA::from_ints(0, 0, 0, 255)
                    } else {
                        RGBA::from_ints(255, 255, 255, 255)
                    }
                } else {
                    theme.text_muted
                };
                let label = format!(" {} ", q.id);
                let tab_style = Style::default()
                    .fg(rgba_color(tab_foreground))
                    .bg(tab_background);
                for (ci, ch) in label.chars().filter(|c| !c.is_control()).enumerate() {
                    let cx = tab_x + ci as u16;
                    if cx >= inner_x + inner_w {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                        cell.set_char(ch);
                        cell.set_style(tab_style);
                    }
                }
                tab_x += label.len() as u16 + 1;
            }
            // Confirm tab
            let confirm_background = if is_confirm {
                rgba_color(theme.accent)
            } else {
                rgba_color(theme.background_panel)
            };
            let confirm_foreground = if is_confirm {
                let (red, green, blue, _) = theme.accent.to_ints();
                let lum =
                    0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue);
                if lum > 128.0 {
                    RGBA::from_ints(0, 0, 0, 255)
                } else {
                    RGBA::from_ints(255, 255, 255, 255)
                }
            } else {
                theme.text_muted
            };
            let confirm_label = " Confirm ";
            let confirm_style = Style::default()
                .fg(rgba_color(confirm_foreground))
                .bg(confirm_background);
            for (ci, ch) in confirm_label.chars().enumerate() {
                let cx = tab_x + ci as u16;
                if cx >= inner_x + inner_w {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                    cell.set_char(ch);
                    cell.set_style(confirm_style);
                }
            }
            y_pos += 1;
            // --- Separator between the tab bar and the content ---
            y_pos += 1;
        }

        // Hidden option rows above/below the viewport (last-resort overflow
        // only). Shown in the footer so folded items never feel "missing".
        let mut opt_overflow: (usize, usize) = (0, 0);

        if is_confirm {
            self.render_review(buf, inner_x, inner_w, y_pos, footer_y, theme);
        } else if let Some(q) = self.questions.get(self.current_tab) {
            // --- Question text region (scrollable) ---
            let opt_rows = self.option_row_count(self.current_tab, inner_w);
            // Rows reserved for the question text (question + purpose hint).
            // The options below it stay fixed and always visible.
            let text_h = footer_y
                .saturating_sub(y_pos)
                .saturating_sub(opt_rows)
                .saturating_sub(1); // 1-row gap above the options
            let mut text_lines: Vec<String> = Self::wrap_text(&q.question, inner_w);
            if let Some(ref purpose) = q.purpose {
                text_lines.extend(Self::wrap_decorated(purpose, "  (", ")", inner_w));
            }
            let max_scroll = text_lines.len().saturating_sub(text_h as usize);
            let scroll = self.text_scroll.min(max_scroll);

            let qstyle = Style::default().fg(rgba_color(theme.text));
            for (ty, line) in (y_pos..).zip(text_lines.iter().skip(scroll).take(text_h as usize)) {
                if ty >= footer_y {
                    break;
                }
                draw_text_line(buf, line, inner_x, ty, inner_w, qstyle);
            }
            let option_y = y_pos + text_h + 1;

            match q.question_type {
                QuestionType::Text => {
                    // Text input field with the project's blinking cursor. The
                    // field scrolls horizontally to keep the cursor visible.
                    let state = self.state.get(self.current_tab);
                    let input = state.map_or(String::new(), |s| s.text_input.clone());
                    let cursor_pos = state.map_or(0, |s| s.cursor_pos);
                    let chars: Vec<char> = input.chars().collect();
                    let field_w = inner_w.saturating_sub(2) as usize; // after "  " prefix
                    let h_scroll = if cursor_pos >= field_w && chars.len() > field_w {
                        cursor_pos - field_w + 1
                    } else {
                        0
                    };
                    // The placeholder stays visible whenever the answer is
                    // empty — even after clicking the field or moving the
                    // cursor back to the start.
                    let show_placeholder = input.is_empty();
                    let input_fg = if show_placeholder {
                        rgba_color(theme.text_muted)
                    } else {
                        rgba_color(theme.text)
                    };
                    let bg = rgba_color(theme.background_panel);
                    let style = Style::default().fg(input_fg).bg(bg);

                    if option_y < footer_y {
                        draw_text_line(buf, "  ", inner_x, option_y, inner_w, style);
                        let text_x = inner_x + 2;
                        let visible: String = if show_placeholder {
                            "Type your answer...".chars().take(field_w).collect()
                        } else {
                            chars.iter().skip(h_scroll).take(field_w).collect()
                        };
                        for (i, ch) in visible.chars().enumerate() {
                            let cx = text_x + i as u16;
                            if cx >= text_x + field_w as u16 {
                                break;
                            }
                            if let Some(cell) = buf.cell_mut((cx, option_y)) {
                                cell.set_char(ch);
                                cell.set_style(style);
                            }
                        }

                        // Blinking cursor at the cursor position. Always visible
                        // while this tab is active — no click required.
                        let cursor_cell = cursor_pos.saturating_sub(h_scroll);
                        let cx = text_x + cursor_cell as u16;
                        if cx <= text_x + field_w as u16
                            && let Some(cell) = buf.cell_mut((cx, option_y))
                        {
                            match self.cursor.current_state(now) {
                                CursorState::On => {
                                    // Invert the cell so the character under
                                    // the cursor stays visible (like the chat
                                    // prompt). At the end of the text this
                                    // renders as a solid block.
                                    cell.set_style(
                                        Style::default().fg(bg).bg(rgba_color(theme.text)),
                                    );
                                }
                                CursorState::Off | CursorState::Blur => {
                                    cell.set_style(
                                        Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
                                    );
                                }
                            }
                        }
                    }
                }
                QuestionType::YesNo | QuestionType::SingleChoice | QuestionType::MultiChoice => {
                    self.render_options(buf, inner_x, inner_w, option_y, footer_y, theme, now);
                    let capacity = footer_y.saturating_sub(option_y) as usize;
                    opt_overflow = self.options_overflow(self.current_tab, inner_w, capacity);
                }
            }
        }

        // --- Footer (keyboard hints, inherits background_panel from the box) ---
        let bg = rgba_color(theme.background_panel);
        let key_fg = rgba_color(theme.text);
        let desc_fg = rgba_color(theme.text_muted);

        let mut fx = inner_x;

        macro_rules! hint {
            ($key:expr, $desc:expr, $gap:expr) => {
                hint!($key, $desc, $gap, 1)
            };
            ($key:expr, $desc:expr, $gap:expr, $key_gap:expr) => {{
                let kw = $key.len() as u16;
                let dw = $desc.len() as u16;
                draw_text_line(
                    buf,
                    $key,
                    fx,
                    footer_y,
                    inner_w.saturating_sub(fx - inner_x),
                    Style::default().fg(key_fg).bg(bg),
                );
                fx += kw + $key_gap;
                draw_text_line(
                    buf,
                    $desc,
                    fx,
                    footer_y,
                    inner_w.saturating_sub(fx - inner_x),
                    Style::default().fg(desc_fg).bg(bg),
                );
                fx += dw + $gap;
            }};
        }

        if tab_count > 1 {
            hint!("⇆", "tab", 1);
        }

        let enter_label = if is_confirm { "submit" } else { "confirm" };
        hint!("enter", enter_label, 1);
        hint!("esc", "dismiss", 0);
        // Last-resort overflow affordance: when option rows fold above or
        // below the viewport, say how many — folded items must read as
        // scrollable, never as missing.
        if !is_confirm {
            let (above, below) = opt_overflow;
            if above > 0 {
                let label = format!("↑{above} more");
                let w = label.chars().count() as u16;
                draw_text_line(
                    buf,
                    &label,
                    fx,
                    footer_y,
                    inner_w.saturating_sub(fx - inner_x),
                    Style::default().fg(desc_fg).bg(bg),
                );
                fx += w + 1;
            }
            if below > 0 {
                let label = format!("↓{below} more");
                draw_text_line(
                    buf,
                    &label,
                    fx,
                    footer_y,
                    inner_w.saturating_sub(fx - inner_x),
                    Style::default().fg(desc_fg).bg(bg),
                );
                fx += label.chars().count() as u16 + 1;
            }
        }
        // Suppress "value assigned to `fx` is never read" warning
        let _ = fx;
    }

    /// Hidden CONTENT rows above/below a viewport of `capacity` flat rows.
    /// Blank gap rows never count — the footer must flag folded items, not
    /// breathing space. `(0, 0)` in the common case: everything fits.
    fn options_overflow(&self, tab: usize, inner_w: u16, capacity: usize) -> (usize, usize) {
        let flat = self.flat_option_rows(tab, inner_w);
        let total = flat.len();
        if total <= capacity {
            return (0, 0);
        }
        let is_content = |r: &&OptFlatRow| !matches!(r, OptFlatRow::Gap { .. });
        let above = self.opt_scroll.min(total);
        let visible = capacity.min(total.saturating_sub(above));
        let above_n = flat[..above].iter().filter(is_content).count();
        let below_n = flat[above + visible..].iter().filter(is_content).count();
        (above_n, below_n)
    }

    /// Render the confirm/review screen. Rows wrap and scroll when the box is
    /// shorter than the content.
    fn render_review(
        &self,
        buf: &mut Buffer,
        inner_x: u16,
        inner_w: u16,
        start_y: u16,
        bottom: u16,
        theme: &Theme,
    ) {
        let styles = ReviewStyles {
            header: Style::default().fg(rgba_color(theme.text)),
            label: Style::default().fg(rgba_color(theme.text_muted)),
            value: Style::default().fg(rgba_color(theme.text)),
            unanswered: Style::default().fg(rgba_color(theme.error)),
            purpose: Style::default().fg(rgba_color(theme.text_muted)),
        };
        let rows = self.review_rows(inner_w, &styles);
        let text_h = bottom.saturating_sub(start_y);
        let max_scroll = rows.len().saturating_sub(text_h as usize);
        let scroll = self.text_scroll.min(max_scroll);
        for (i, spans) in rows.iter().enumerate().skip(scroll).take(text_h as usize) {
            let ry = start_y + i as u16 - scroll as u16;
            if ry >= bottom {
                break;
            }
            let mut sx = inner_x;
            for (text, style) in spans {
                draw_text_line(
                    buf,
                    text,
                    sx,
                    ry,
                    inner_w.saturating_sub(sx - inner_x),
                    *style,
                );
                sx += text.chars().count() as u16;
            }
        }
    }

    /// Build the review screen's display rows: header, per-question label +
    /// wrapped answer, and wrapped purpose hints. Each row is a list of styled
    /// spans so the muted label and the colored value stay distinct.
    fn review_rows(&self, inner_w: u16, styles: &ReviewStyles) -> Vec<Vec<(String, Style)>> {
        let mut out: Vec<Vec<(String, Style)>> = Vec::new();
        out.push(vec![("Review your answers:".to_string(), styles.header)]);
        for (i, q) in self.questions.iter().enumerate() {
            let label = format!(" {}: ", q.id);
            let label_w = label.chars().count() as u16;
            let val_w = inner_w.saturating_sub(label_w);
            let display = self.question_summary(i);
            let val_style = if self.state.get(i).is_some_and(|s| s.answered) {
                styles.value
            } else {
                styles.unanswered
            };
            let val_lines = Self::wrap_text(&display, val_w);
            let pad = " ".repeat(label_w as usize);
            for (li, vline) in val_lines.into_iter().enumerate() {
                if li == 0 {
                    out.push(vec![(label.clone(), styles.label), (vline, val_style)]);
                } else {
                    out.push(vec![(format!("{pad}{vline}"), val_style)]);
                }
            }
            if let Some(ref purpose) = q.purpose {
                for pl in Self::wrap_decorated(purpose, "  └ ", "", inner_w) {
                    out.push(vec![(pl, styles.purpose)]);
                }
            }
        }
        out
    }

    /// Render YesNo / SingleChoice / MultiChoice options inside a viewport of
    /// `bottom - start_y` rows. The focused row uses the standard `🞴`
    /// marker (same pattern as the permission box) — no background wash —
    /// and a committed-but-unfocused row keeps its `🞴` in the accent color,
    /// so focus and previous answer stay distinguishable. The custom row uses
    /// the same markers (no pencil) to keep the list uniform. When the flat
    /// rows exceed the viewport (last-resort overflow only — the box grows
    /// first), the window auto-follows `selected_row`.
    #[allow(clippy::too_many_arguments)]
    fn render_options(
        &mut self,
        buf: &mut Buffer,
        inner_x: u16,
        inner_w: u16,
        start_y: u16,
        bottom: u16,
        theme: &Theme,
        now: SystemTime,
    ) {
        let Some(q) = self.questions.get(self.current_tab).cloned() else {
            return;
        };
        let tab = self.current_tab;
        let capacity = bottom.saturating_sub(start_y) as usize;
        self.ensure_selected_visible(tab, inner_w, capacity);
        let flat = self.flat_option_rows(tab, inner_w);
        let opt_w = inner_w.saturating_sub(3);

        // Absolute flat index of the first visible row, to detect each
        // option's first wrapped line (the only one carrying the marker).
        let base = self.opt_scroll;
        for (vi, (ai, frow)) in flat
            .iter()
            .enumerate()
            .skip(base)
            .take(capacity)
            .enumerate()
        {
            let ry = start_y + vi as u16;
            match frow {
                OptFlatRow::CustomInput => {
                    self.render_custom_input(buf, inner_x, inner_w, ry, theme, now);
                }
                // Breathing row: leave the panel background alone so the
                // options above and below read as separate items.
                OptFlatRow::Gap { .. } => {}
                OptFlatRow::Label { row: i, text: line } => {
                    let i = *i;
                    let is_focused = i == self.selected_row;
                    // First wrapped line of the option: the previous flat row
                    // belongs to a different option (or is the input line, or
                    // there is no previous row).
                    let is_first_line = !matches!(
                        ai.checked_sub(1).and_then(|p| flat.get(p)),
                        Some(OptFlatRow::Label { row: prev, .. }) if *prev == i
                    );

                    let (indicator, indicator_color) = match q.question_type {
                        QuestionType::YesNo | QuestionType::SingleChoice => {
                            let is_committed = self
                                .state
                                .get(tab)
                                .is_some_and(|s| s.single_selection == Some(i));
                            let marked = is_focused || is_committed;
                            let ind = if marked { "🞴 " } else { "  " };
                            let col = if is_committed {
                                theme.accent
                            } else if is_focused {
                                theme.secondary
                            } else {
                                theme.text_muted
                            };
                            (ind, col)
                        }
                        QuestionType::MultiChoice => {
                            let is_checked = self
                                .state
                                .get(tab)
                                .is_some_and(|s| s.multi_selection.contains(&i));
                            let ind = if is_checked { "☑ " } else { "☐ " };
                            let col = if is_checked {
                                theme.accent
                            } else if is_focused {
                                theme.secondary
                            } else {
                                theme.text_muted
                            };
                            (ind, col)
                        }
                        QuestionType::Text => unreachable!("Text questions have no options"),
                    };

                    if is_first_line {
                        draw_text_line(
                            buf,
                            indicator,
                            inner_x,
                            ry,
                            inner_w,
                            Style::default().fg(rgba_color(indicator_color)),
                        );
                    }
                    let opt_fg = if is_focused {
                        theme.secondary
                    } else {
                        theme.text
                    };
                    draw_text_line(
                        buf,
                        line,
                        inner_x + 3,
                        ry,
                        opt_w,
                        Style::default().fg(rgba_color(opt_fg)),
                    );
                }
            }
        }
    }

    /// Render the SingleChoice custom-answer input line (single-line field
    /// with placeholder + blinking cursor, mirroring the Text tab).
    fn render_custom_input(
        &self,
        buf: &mut Buffer,
        inner_x: u16,
        inner_w: u16,
        y: u16,
        theme: &Theme,
        now: SystemTime,
    ) {
        let state = self.state.get(self.current_tab);
        let input = state.map_or(String::new(), |s| s.custom_text.clone());
        let cursor_pos = state.map_or(0, |s| s.custom_cursor);
        let chars: Vec<char> = input.chars().collect();
        let field_w = inner_w.saturating_sub(2) as usize; // after "  " prefix
        let h_scroll = if cursor_pos >= field_w && chars.len() > field_w {
            cursor_pos - field_w + 1
        } else {
            0
        };
        let show_placeholder = input.is_empty();
        let input_fg = if show_placeholder {
            rgba_color(theme.text_muted)
        } else {
            rgba_color(theme.text)
        };
        let bg = rgba_color(theme.background_panel);
        let style = Style::default().fg(input_fg).bg(bg);

        draw_text_line(buf, "  ", inner_x, y, inner_w, style);
        let text_x = inner_x + 2;
        let visible: String = if show_placeholder {
            CUSTOM_PLACEHOLDER.chars().take(field_w).collect()
        } else {
            chars.iter().skip(h_scroll).take(field_w).collect()
        };
        for (i, ch) in visible.chars().enumerate() {
            let cx = text_x + i as u16;
            if cx >= text_x + field_w as u16 {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        // Blinking cursor only while the custom row is focused; otherwise the
        // typed draft stays visible without stealing the blink.
        if self.is_custom_focused() {
            let cursor_cell = cursor_pos.saturating_sub(h_scroll);
            let cx = text_x + cursor_cell as u16;
            if cx <= text_x + field_w as u16
                && let Some(cell) = buf.cell_mut((cx, y))
            {
                match self.cursor.current_state(now) {
                    CursorState::On => {
                        cell.set_style(Style::default().fg(bg).bg(rgba_color(theme.text)));
                    }
                    CursorState::Off | CursorState::Blur => {
                        cell.set_style(Style::default().fg(rgba_color(theme.text_muted)).bg(bg));
                    }
                }
            }
        }
    }

    /// Build a one-line summary of the answer for a question (used in the review screen).
    fn question_summary(&self, index: usize) -> String {
        let Some(q) = self.questions.get(index) else {
            return "(unknown)".into();
        };
        let Some(s) = self.state.get(index) else {
            return "(not answered)".into();
        };

        if !s.answered
            && s.text_input.is_empty()
            && s.custom_text.is_empty()
            && s.single_selection.is_none()
            && s.multi_selection.is_empty()
        {
            return "(not answered)".into();
        }

        match q.question_type {
            QuestionType::Text => {
                if s.text_input.is_empty() {
                    "(not answered)".into()
                } else {
                    s.text_input.clone()
                }
            }
            QuestionType::YesNo => match s.single_selection {
                Some(0) => "Yes".into(),
                Some(1) => "No".into(),
                _ => "(not answered)".into(),
            },
            QuestionType::SingleChoice => {
                let custom = q.options.as_ref().map_or(0, Vec::len);
                if s.single_selection == Some(custom) {
                    if s.custom_text.is_empty() {
                        "(type your custom answer...)".into()
                    } else {
                        s.custom_text.clone()
                    }
                } else {
                    s.single_selection
                        .and_then(|i| q.options.as_ref()?.get(i).cloned())
                        .unwrap_or_else(|| {
                            if s.custom_text.is_empty() {
                                "(not answered)".into()
                            } else {
                                // Typed but not committed yet: preview the draft.
                                s.custom_text.clone()
                            }
                        })
                }
            }
            QuestionType::MultiChoice => {
                let selected: Vec<&str> = s
                    .multi_selection
                    .iter()
                    .filter_map(|&i| q.options.as_ref()?.get(i).map(std::string::String::as_str))
                    .collect();
                if selected.is_empty() {
                    "(none selected)".into()
                } else {
                    selected.join(", ")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use cosh_tools::question::types::{QuestionItem, QuestionType};
    use crossterm::event::KeyCode;

    use super::QuestionDialog;

    fn wrap(text: &str, width: u16) -> Vec<String> {
        QuestionDialog::wrap_text(text, width)
    }

    fn question(
        id: &str,
        question_type: QuestionType,
        options: Option<Vec<String>>,
    ) -> QuestionItem {
        QuestionItem {
            id: id.to_string(),
            question: "test".to_string(),
            question_type,
            purpose: None,
            options,
            required: true,
            recommended: None,
        }
    }

    #[test]
    fn wrap_short_text_stays_on_one_line() {
        assert_eq!(wrap("hello world", 40), vec!["hello world"]);
        assert_eq!(wrap("", 40), vec![""]);
    }

    #[test]
    fn wrap_breaks_at_word_boundaries() {
        assert_eq!(wrap("hello world foo", 11), vec!["hello world", "foo"]);
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
    }

    #[test]
    fn wrap_collapses_whitespace_runs() {
        assert_eq!(wrap("a   b\t c", 10), vec!["a b c"]);
        assert_eq!(
            wrap("  leading and trailing  ", 40),
            vec!["leading and trailing"]
        );
    }

    #[test]
    fn wrap_hard_splits_long_words() {
        assert_eq!(
            wrap("supercalifragilistic", 5),
            vec!["super", "calif", "ragil", "istic"]
        );
        // A long word combined with a shorter one (10-char chunks)
        assert_eq!(
            wrap("hi supercalifragilistic", 10),
            vec!["hi", "supercalif", "ragilistic"]
        );
    }

    #[test]
    fn wrap_respects_literal_newlines() {
        assert_eq!(wrap("one\ntwo", 40), vec!["one", "two"]);
        assert_eq!(wrap("a\n\nb", 40), vec!["a", "", "b"]);
        // Trailing newline does not add a trailing blank row
        assert_eq!(wrap("one\n", 40), vec!["one"]);
    }

    #[test]
    fn wrap_drops_control_characters() {
        assert_eq!(wrap("a\u{1b}[31mb", 40), vec!["a[31mb"]);
    }

    #[test]
    fn wrap_narrow_width_never_exceeds_limit() {
        for w in 1..=3 {
            for line in wrap("hello world", w) {
                assert!(
                    line.chars().count() <= w as usize,
                    "{line:?} wider than {w}"
                );
            }
        }
    }

    #[test]
    fn wrap_unicode_is_char_aware() {
        assert_eq!(wrap("olá mundo", 5), vec!["olá", "mundo"]);
    }

    #[test]
    fn text_question_occupies_one_option_row() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        // The input field must count as one option row so it lands above the
        // footer and stays clickable.
        assert_eq!(d.option_row_count(0, 40), 1);
        assert_eq!(d.option_wrapped_lines(0, 40).len(), 1);
    }

    #[test]
    fn option_row_count_accounts_for_wrapping() {
        let mut d = QuestionDialog::new();
        let long = "x".repeat(80);
        d.show_questions(vec![question(
            "s1",
            QuestionType::SingleChoice,
            Some(vec![long]),
        )]);
        // 80 chars at width 37 (40 - 3 indicator column) → 3 wrapped rows,
        // plus the TUI-owned virtual custom row (1 wrapped row), its
        // always-visible input line (+1), the breathing gap between the two
        // blocks (+1) and the trailing gap below the input (+1).
        assert_eq!(d.option_row_count(0, 40), 7);
        assert_eq!(d.option_wrapped_lines(0, 40).len(), 2);
        assert_eq!(d.option_wrapped_lines(0, 40)[0].len(), 3);
    }

    #[test]
    fn text_question_input_lands_above_footer() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        let width = 60u16;
        let height = d.required_height(width);
        // Layout math replicated from `render`: area.y = 0, single question
        // (no tab bar, hence no separator — just the top pad), footer at
        // height-2.
        let footer_y = height.saturating_sub(2);
        let y_pos = 1u16;
        let opt_rows = d.option_row_count(0, width.saturating_sub(5));
        let text_h = footer_y
            .saturating_sub(y_pos)
            .saturating_sub(opt_rows)
            .saturating_sub(1);
        let option_y = y_pos + text_h + 1;
        assert!(
            option_y < footer_y,
            "input field at {option_y} must sit above the footer at {footer_y}"
        );
    }

    #[test]
    fn text_input_edits_at_cursor() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);

        d.handle_key(KeyCode::Char('a'));
        d.handle_key(KeyCode::Char('b'));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("ab"));

        // Move to the start and insert "X".
        d.handle_key(KeyCode::Left);
        d.handle_key(KeyCode::Left);
        d.handle_key(KeyCode::Char('X'));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("Xab"));

        // Backspace removes the char before the cursor.
        d.handle_key(KeyCode::Backspace);
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("ab"));

        // End + Delete removes the last char.
        d.handle_key(KeyCode::End);
        d.handle_key(KeyCode::Left);
        d.handle_key(KeyCode::Delete);
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("a"));

        // Home moves back to the start; Left clamps at the boundary.
        d.handle_key(KeyCode::Home);
        d.handle_key(KeyCode::Left);
        d.handle_key(KeyCode::Char('b'));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("ba"));
    }

    #[test]
    fn text_input_types_immediately_without_clicking() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        // The field is active as soon as its tab is shown: typing works
        // without clicking or entering an edit mode first.
        d.handle_key(KeyCode::Char('o'));
        d.handle_key(KeyCode::Char('l'));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("ol"));
    }

    #[test]
    fn text_input_enter_commits_and_advances() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![
            question("t1", QuestionType::Text, None),
            question("s2", QuestionType::YesNo, None),
        ]);
        d.handle_key(KeyCode::Char('y')); // types 'y' immediately
        d.handle_key(KeyCode::Enter); // commit, move to next tab
        assert_eq!(d.current_tab, 1);
        assert!(d.state[0].answered);
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("y"));
    }

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::CONTROL)
    }

    #[test]
    fn text_input_ctrl_word_movement() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        for ch in "hello world foo".chars() {
            d.handle_key_event(key(KeyCode::Char(ch)));
        }
        // End: cursor after "foo". Ctrl+Left jumps to "foo".
        d.handle_key_event(ctrl_key(KeyCode::Left));
        assert_eq!(d.state[0].cursor_pos, 12);
        // Ctrl+Left again jumps to "world".
        d.handle_key_event(ctrl_key(KeyCode::Left));
        assert_eq!(d.state[0].cursor_pos, 6);
        // Ctrl+Left again jumps to "hello".
        d.handle_key_event(ctrl_key(KeyCode::Left));
        assert_eq!(d.state[0].cursor_pos, 0);
        // Ctrl+Right jumps to "world".
        d.handle_key_event(ctrl_key(KeyCode::Right));
        assert_eq!(d.state[0].cursor_pos, 6);
        // Ctrl+Right jumps to "foo".
        d.handle_key_event(ctrl_key(KeyCode::Right));
        assert_eq!(d.state[0].cursor_pos, 12);
    }

    #[test]
    fn text_input_ctrl_deletes_word_before_cursor() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        for ch in "hello world foo".chars() {
            d.handle_key_event(key(KeyCode::Char(ch)));
        }
        // Cursor after "foo". Ctrl+Backspace deletes "foo".
        d.handle_key_event(ctrl_key(KeyCode::Backspace));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("hello world "));
        // Ctrl+W deletes "world".
        d.handle_key_event(ctrl_key(KeyCode::Char('w')));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("hello "));
        // Ctrl+Backspace deletes "hello".
        d.handle_key_event(ctrl_key(KeyCode::Backspace));
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some(""));
    }

    #[test]
    fn text_input_paste_inserts_at_cursor() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        d.handle_key_event(key(KeyCode::Char('a')));
        d.handle_key_event(key(KeyCode::Char('b')));
        // Cursor between 'a' and 'b' → paste "XY" → "aXYb".
        d.handle_key_event(key(KeyCode::Left));
        d.handle_paste("XY");
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some("aXYb"));
        // Pasting on a non-Text tab is ignored.
        d.show_questions(vec![question("s1", QuestionType::YesNo, None)]);
        d.handle_paste("ignored");
        assert_eq!(d.build_answers()[0].answer, None);
    }

    #[test]
    fn text_input_placeholder_returns_when_cleared() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![question("t1", QuestionType::Text, None)]);
        // Type something, then delete it all: the input ends empty, so the
        // placeholder must be visible again (render uses input.is_empty()).
        d.handle_key(KeyCode::Char('x'));
        d.handle_key(KeyCode::Backspace);
        assert_eq!(d.build_answers()[0].answer.as_deref(), Some(""));
        assert!(d.state[0].text_input.is_empty());
    }

    #[test]
    fn decorated_hint_single_line() {
        assert_eq!(
            QuestionDialog::wrap_decorated("why?", "  (", ")", 40),
            vec!["  (why?)"]
        );
    }

    #[test]
    fn decorated_hint_wraps_with_indent_and_suffix() {
        let lines = QuestionDialog::wrap_decorated(
            "this purpose text is far too long for the box width",
            "  (",
            ")",
            20,
        );
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("  ("));
        assert!(lines[1].starts_with("  "));
        assert!(lines[2].ends_with(')'));
        // Continuation lines align under the prefix.
        assert_eq!(lines[1].chars().take(2).collect::<String>(), "  ");
    }

    fn single_choice(id: &str, options: Vec<&str>, recommended: Option<&str>) -> QuestionItem {
        QuestionItem {
            id: id.to_string(),
            question: "pick?".to_string(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(options.into_iter().map(str::to_string).collect()),
            required: true,
            recommended: recommended.map(str::to_string),
        }
    }

    #[test]
    fn single_choice_always_has_custom_row() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        // 2 model options + 1 TUI-owned custom row.
        assert_eq!(d.row_count(0), 3);
        assert_eq!(d.custom_index(0), Some(2));
        let wrapped = d.option_wrapped_lines(0, 40);
        assert_eq!(wrapped.len(), 3);
        assert_eq!(wrapped[2], vec![super::CUSTOM_RESPONSE_LABEL.to_string()]);
        // Other types never get the custom row.
        d.show_questions(vec![question(
            "m1",
            QuestionType::MultiChoice,
            Some(vec!["A".into()]),
        )]);
        assert_eq!(d.custom_index(0), None);
        assert_eq!(d.row_count(0), 1);
        d.show_questions(vec![question("y1", QuestionType::YesNo, None)]);
        assert_eq!(d.custom_index(0), None);
        assert_eq!(d.row_count(0), 2);
    }

    #[test]
    fn single_choice_recommended_gets_badge() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice(
            "s1",
            vec!["Deltas", "Snapshots"],
            Some("Snapshots"),
        )]);
        let wrapped = d.option_wrapped_lines(0, 40);
        assert_eq!(wrapped[0], vec!["Deltas".to_string()]);
        assert_eq!(wrapped[1], vec!["Snapshots (Recommended)".to_string()]);
        // Badge matches by value even when the payload was not normalized
        // (recommended not first) — the tool normally sorts it to the top.
        assert_eq!(
            QuestionDialog::display_option(&d.questions[0], "Snapshots"),
            "Snapshots (Recommended)"
        );
        assert_eq!(
            QuestionDialog::display_option(&d.questions[0], "Deltas"),
            "Deltas"
        );
    }

    #[test]
    fn single_choice_regular_selection_still_works() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        // Highlight first row (regular option) + Enter commits it.
        assert_eq!(d.selected_row, 0);
        d.handle_key(KeyCode::Enter);
        let answers = d.build_answers();
        assert_eq!(answers[0].selected, Some(vec!["A".to_string()]));
        assert_eq!(d.question_summary(0), "A");
    }

    #[test]
    fn single_choice_custom_typing_commits_free_text() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        // Move to the virtual custom row (last).
        d.handle_key(KeyCode::Down);
        d.handle_key(KeyCode::Down);
        assert_eq!(d.selected_row, 2);
        assert!(d.is_custom_focused());
        // Typing goes to the custom buffer, not to navigation.
        for ch in "my way".chars() {
            d.handle_key(KeyCode::Char(ch));
        }
        assert_eq!(d.state[0].custom_text, "my way");
        // Enter commits the custom answer and it travels as the selection.
        d.handle_key(KeyCode::Enter);
        let answers = d.build_answers();
        assert_eq!(answers[0].selected, Some(vec!["my way".to_string()]));
        assert_eq!(d.question_summary(0), "my way");
    }

    #[test]
    fn single_choice_custom_empty_counts_as_unanswered() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        d.handle_key(KeyCode::Down);
        d.handle_key(KeyCode::Down);
        d.handle_key(KeyCode::Enter); // commit with no text typed
        let answers = d.build_answers();
        assert_eq!(answers[0].selected, None);
        assert!(d.question_summary(0).contains("custom"));
    }

    #[test]
    fn single_choice_custom_input_row_counts_in_height() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A"], Some("A"))]);
        // The box starts pre-expanded: 1 option + gap + custom label + its
        // input line + trailing gap below the input, upfront — focusing the
        // custom row shifts nothing.
        assert_eq!(d.option_row_count(0, 40), 5);
        d.handle_key(KeyCode::Down);
        assert_eq!(d.option_row_count(0, 40), 5);
    }

    #[test]
    fn single_choice_custom_backspace_and_cursor() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        d.handle_key(KeyCode::Down);
        d.handle_key(KeyCode::Down);
        d.handle_key(KeyCode::Char('a'));
        d.handle_key(KeyCode::Char('b'));
        d.handle_key(KeyCode::Left);
        d.handle_key(KeyCode::Char('X'));
        assert_eq!(d.state[0].custom_text, "aXb");
        d.handle_key(KeyCode::Backspace);
        assert_eq!(d.state[0].custom_text, "ab");
    }

    #[test]
    fn single_choice_custom_paste() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A"], Some("A"))]);
        d.handle_key(KeyCode::Down); // focus custom
        d.handle_paste("hello\nworld");
        assert_eq!(d.state[0].custom_text, "helloworld");
    }

    #[test]
    fn single_choice_number_key_jumps_to_regular_option() {
        let mut d = QuestionDialog::new();
        d.show_questions(vec![
            single_choice("s1", vec!["A", "B"], Some("A")),
            question("t2", QuestionType::Text, None),
        ]);
        // '2' selects the second regular option and advances to the next tab.
        d.handle_key(KeyCode::Char('2'));
        assert_eq!(d.current_tab, 1);
        // First question committed to "B" (answer built even off-tab).
        // Re-open to inspect: build_answers reflects the committed selection.
        assert_eq!(d.build_answers()[0].selected, Some(vec!["B".to_string()]));
    }

    // --- Rendering helpers (buffer assertions) ---------------------------

    fn test_theme() -> crate::theme::Theme {
        crate::theme::ThemeRegistry::new().default_theme().clone()
    }

    fn screen_text(buf: &ratatui::buffer::Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn grow_first_custom_row_visible_without_scroll() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // Realistic payload: 3 options + TUI custom row, ample terminal.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice(
            "s1",
            vec!["Deltas because x", "Snapshots because y", "Neither"],
            Some("Snapshots because y"),
        )]);
        let theme = test_theme();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        d.render(&mut buf, area, &theme, std::time::SystemTime::now());

        // No last-resort scrolling: everything fits, offset stays 0.
        assert_eq!(d.opt_scroll, 0);
        let screen = screen_text(&buf);
        // Recommended badge on top area, custom row at the bottom — no
        // scrolling needed to discover either. The box starts pre-expanded:
        // the custom input (placeholder) is already on screen, nothing only
        // appears after navigating first.
        assert!(screen.contains("Snapshots because y (Recommended)"));
        assert!(screen.contains(super::CUSTOM_RESPONSE_LABEL));
        assert!(
            screen.contains(super::CUSTOM_PLACEHOLDER),
            "custom input must be visible upfront:\n{screen}"
        );
        // Uniform markers: the pencil glyph is gone.
        assert!(!screen.contains('✎'));
        // Focus uses the standard marker, not a background wash: no cell may
        // carry the wash color.
        assert!(screen.contains("🞴"));
        let wash = crate::theme::rgba_color(theme.background_element);
        for cell in buf.content() {
            assert_ne!(
                cell.bg, wash,
                "option rows must not use a background highlight"
            );
        }
    }

    #[test]
    fn overflow_viewport_follows_focus_to_custom_row() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // 10 long options: flat rows far exceed a tiny viewport.
        let opts: Vec<String> = (1..=10)
            .map(|i| format!("Option {i} with quite a lot of explanatory text padded to wrap"))
            .collect();
        let mut d = QuestionDialog::new();
        d.show_questions(vec![QuestionItem {
            id: "big".to_string(),
            question: "pick?".to_string(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(opts),
            required: true,
            recommended: None,
        }]);
        let theme = test_theme();
        let area = Rect::new(0, 0, 40, 14);

        // Focused top: no scroll yet, custom row below the fold — and the
        // footer says so (folded items read as scrollable, never missing).
        let mut buf = Buffer::empty(area);
        d.render(&mut buf, area, &theme, std::time::SystemTime::now());
        assert_eq!(d.opt_scroll, 0);
        let screen = screen_text(&buf);
        assert!(!screen.contains(super::CUSTOM_RESPONSE_LABEL));
        assert!(
            screen.contains('↓'),
            "footer must flag folded rows:\n{screen}"
        );
        assert!(screen.contains("more"));
        assert!(!screen.contains('↑'));

        // Walk the focus down to the virtual custom row: the viewport must
        // follow so the focused row (and its input) stays visible.
        for _ in 0..10 {
            d.handle_key(KeyCode::Down);
        }
        assert_eq!(d.selected_row, 10);
        let mut buf2 = Buffer::empty(area);
        d.render(&mut buf2, area, &theme, std::time::SystemTime::now());
        assert!(
            d.opt_scroll > 0,
            "overflow must scroll instead of stranding the focus"
        );
        let screen2 = screen_text(&buf2);
        assert!(screen2.contains(super::CUSTOM_RESPONSE_LABEL));
        assert!(screen2.contains(super::CUSTOM_PLACEHOLDER));
        assert!(
            screen2.contains('↑'),
            "scrolled viewport must flag rows above"
        );
        assert!(!screen2.contains('↓'), "custom block ends the list");
    }

    #[test]
    fn gaps_separate_question_and_options() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // Minimal payload: question (1 row), no purpose, two 1-row options.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        // Override the visible question text with a short one-liner.
        d.questions[0].question = "Q?".to_string();
        let theme = test_theme();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        d.render(&mut buf, area, &theme, std::time::SystemTime::now());
        let screen = screen_text(&buf);
        let rows: Vec<&str> = screen.split('\n').collect();
        // Layout (single question: top pad only, no separator): row 0 top
        // pad, row 1 question text, row 2 exactly one blank gap row, row 3
        // first option, row 4 breathing gap, row 5 second option.
        assert!(
            rows[1].contains("Q?"),
            "question text at row 1:\n{}",
            rows[1]
        );
        assert_eq!(
            rows[2].trim(),
            "┃",
            "exactly one blank gap row before the options"
        );
        assert!(rows[3].contains('A'), "first option right after the gap");
        assert_eq!(
            rows[4].trim(),
            "┃",
            "breathing gap between options so each item reads apart"
        );
        assert!(rows[5].contains('B'), "second option after its gap");
        // Trailing breathing gap below the custom input: it must never glue
        // to the footer hints.
        assert!(rows[8].contains("Type your custom answer..."));
        assert_eq!(rows[9].trim(), "┃", "bottom gap below the input");
        assert!(rows[10].contains("enter"), "footer hints after the gap");
    }

    #[test]
    fn required_height_grows_to_fit_without_absolute_cap() {
        // Growth contract: the box fits its content exactly for as long as
        // the screen allows — no absolute cap may fold items away while free
        // space remains (only the App-level responsive clamp may scroll).
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A"], Some("A"))]);
        // content = question(1) + options(A + gap + custom + always-on
        // input + trailing gap = 5) = 6
        // → chrome(3) + tabs(0) + content(6) + footer(1) = 10.
        assert_eq!(d.required_height(80), 10);

        // Huge payload: 30 one-row options interleaved with 30 gap rows +
        // custom + input + trailing gap = 63 option rows,
        // content = 1 + 63 = 64 → uncapped: 3 + 64 + 1 = 68.
        let many: Vec<String> = (0..30).map(|i| format!("Option {i}")).collect();
        let mut d2 = QuestionDialog::new();
        d2.show_questions(vec![QuestionItem {
            id: "big".to_string(),
            question: "pick?".to_string(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(many),
            required: true,
            recommended: None,
        }]);
        assert_eq!(d2.required_height(80), 3 + 64 + 1);
    }

    #[test]
    fn committed_and_focused_rows_share_standard_marker() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // Commit row 1, then navigate back: focus (row 0) and previous
        // answer (row 1) both carry 🞴 — focus in secondary, answer in
        // accent — with no background wash and no pencil.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![
            single_choice("s1", vec!["A", "B"], Some("A")),
            question("t2", QuestionType::Text, None),
        ]);
        d.handle_key(KeyCode::Down); // focus B
        d.handle_key(KeyCode::Enter); // commit B, advance to t2
        assert_eq!(d.current_tab, 1);
        // Back to s1 via Tab (Left/Right edit the Text tab's cursor).
        d.handle_key(KeyCode::Tab); // t2 -> confirm
        d.handle_key(KeyCode::Tab); // confirm -> s1 (focus resets to 0)
        assert_eq!(d.current_tab, 0);
        assert_eq!(d.selected_row, 0);

        let theme = test_theme();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        d.render(&mut buf, area, &theme, std::time::SystemTime::now());
        let screen = screen_text(&buf);
        assert_eq!(
            screen.matches("🞴").count(),
            2,
            "focus and previous answer must both use the standard marker:\n{screen}"
        );
        assert!(!screen.contains('✎'));
        let wash = crate::theme::rgba_color(theme.background_element);
        for cell in buf.content() {
            assert_ne!(cell.bg, wash);
        }
        // The committed answer still builds correctly.
        assert_eq!(d.build_answers()[0].selected, Some(vec!["B".to_string()]));
    }

    #[test]
    fn mouse_click_selects_option_and_focuses_custom() {
        use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
        use ratatui::layout::Rect;
        // Clicking a regular row commits it (advances); clicking the custom
        // row only focuses it so the user can type.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        let area = Rect::new(0, 0, 80, 24);
        let h = d.required_height(area.width);
        let inner_w = area.width.saturating_sub(5);
        let footer_y = area.y + h.saturating_sub(2);
        // Single-question dialogs in these tests draw no tab bar and no
        // separator — just the single top pad row (mirrors `render`).
        let start_y = area.y + 1;
        let opt_rows = d.option_row_count(0, inner_w);
        let text_h = footer_y
            .saturating_sub(start_y)
            .saturating_sub(opt_rows)
            .saturating_sub(1);
        let option_y = start_y + text_h + 1;

        // Flat rows (single-line options): A(+0), gap(+1), B(+2), gap(+3),
        // custom(+4), input(+5).
        let click_b = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            area.x + 5,
            option_y + 2,
            MouseModifiers::none(),
        );
        assert!(d.handle_mouse(&click_b, area));
        // Single question + confirm tab → advances to confirm.
        assert_eq!(d.current_tab, 1);
        assert_eq!(d.build_answers()[0].selected, Some(vec!["B".to_string()]));

        // Fresh dialog: clicking the breathing gap after "A" acts as "A"
        // (generous click target).
        let mut dg = QuestionDialog::new();
        dg.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        let click_gap = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            area.x + 5,
            option_y + 1,
            MouseModifiers::none(),
        );
        assert!(dg.handle_mouse(&click_gap, area));
        assert_eq!(dg.current_tab, 1);
        assert_eq!(dg.build_answers()[0].selected, Some(vec!["A".to_string()]));

        // Fresh dialog: click the virtual custom row (index 2).
        let mut d2 = QuestionDialog::new();
        d2.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        let h2 = d2.required_height(area.width);
        let footer_y2 = area.y + h2.saturating_sub(2);
        let opt_rows2 = d2.option_row_count(0, inner_w);
        let text_h2 = footer_y2
            .saturating_sub(start_y)
            .saturating_sub(opt_rows2)
            .saturating_sub(1);
        let option_y2 = start_y + text_h2 + 1;
        let click_custom = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            area.x + 5,
            option_y2 + 4,
            MouseModifiers::none(),
        );
        assert!(d2.handle_mouse(&click_custom, area));
        // Focused, not committed: still on the tab, nothing answered yet.
        assert_eq!(d2.current_tab, 0);
        assert_eq!(d2.selected_row, 2);
        assert!(d2.is_custom_focused());

        // Click the input line right below: places the cursor.
        let click_input = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            area.x + 5,
            option_y2 + 5,
            MouseModifiers::none(),
        );
        assert!(d2.handle_mouse(&click_input, area));
        assert_eq!(d2.selected_row, 2);
    }

    // --- Mouse-wheel routing -------------------------------------------

    /// First options row for a single-question dialog, mirroring the dialog
    /// layout (same formula as `render`/`handle_mouse`).
    fn test_option_y(d: &QuestionDialog, area: ratatui::layout::Rect) -> u16 {
        let inner_w = area.width.saturating_sub(5);
        let height = d.required_height(area.width).min(area.height);
        let footer_y = area.y + height.saturating_sub(2);
        // Single-question dialogs in these tests draw no tab bar.
        // Single-question dialogs in these tests draw no tab bar and no
        // separator — just the single top pad row (mirrors `render`).
        let start_y = area.y + 1;
        let opt_rows = d.option_row_count(0, inner_w);
        let text_h = footer_y
            .saturating_sub(start_y)
            .saturating_sub(opt_rows)
            .saturating_sub(1);
        start_y + text_h + 1
    }

    #[test]
    fn wheel_hidden_dialog_consumes_nothing() {
        use ratatui::layout::Rect;
        let mut d = QuestionDialog::new();
        assert!(!d.visible);
        assert!(!d.handle_wheel(5, Rect::new(0, 0, 80, 24), true));
    }

    #[test]
    fn wheel_over_options_moves_focus_without_wrapping() {
        use ratatui::layout::Rect;
        let mut d = QuestionDialog::new();
        d.show_questions(vec![single_choice("s1", vec!["A", "B"], Some("A"))]);
        let area = Rect::new(0, 0, 80, 24);
        let option_y = test_option_y(&d, area);

        // Walk down through the options (A, B, custom): focus-only, no
        // commit, no tab change.
        assert!(d.handle_wheel(option_y, area, true));
        assert_eq!(d.selected_row, 1);
        assert!(d.handle_wheel(option_y, area, true));
        assert_eq!(d.selected_row, 2);
        assert!(d.is_custom_focused());
        // Clamped at the last row: unlike Down, the wheel never wraps.
        assert!(d.handle_wheel(option_y, area, true));
        assert_eq!(d.selected_row, 2);
        assert!(d.state[0].single_selection.is_none());
        assert_eq!(d.current_tab, 0);

        // Walk back up, clamped at the first row.
        assert!(d.handle_wheel(option_y, area, false));
        assert_eq!(d.selected_row, 1);
        assert!(d.handle_wheel(option_y, area, false));
        assert_eq!(d.selected_row, 0);
        assert!(d.handle_wheel(option_y, area, false));
        assert_eq!(d.selected_row, 0);
    }

    #[test]
    fn wheel_outside_options_scrolls_question_text() {
        use ratatui::layout::Rect;
        // Long purpose (overflowing text) + short options in a short box:
        // text must scroll while the options stay put.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![QuestionItem {
            id: "s1".to_string(),
            question: "pick?".to_string(),
            question_type: QuestionType::SingleChoice,
            purpose: Some("porque ".repeat(100)),
            options: Some(vec!["A".to_string(), "B".to_string()]),
            required: true,
            recommended: Some("A".to_string()),
        }]);
        let area = Rect::new(0, 0, 80, 14);
        let option_y = test_option_y(&d, area);
        assert_eq!(d.text_scroll, 0);

        // Wheel over the question-text region scrolls the text...
        assert!(d.handle_wheel(option_y - 2, area, true));
        assert_eq!(d.text_scroll, super::SCROLL_STEP);
        assert_eq!(d.selected_row, 0);
        // ...while wheel over the options moves focus and leaves the text.
        assert!(d.handle_wheel(option_y, area, true));
        assert_eq!(d.selected_row, 1);
        assert_eq!(d.text_scroll, super::SCROLL_STEP);
        assert!(d.handle_wheel(option_y, area, false));
        assert_eq!(d.selected_row, 0);
        assert_eq!(d.text_scroll, super::SCROLL_STEP);
    }

    #[test]
    fn wheel_on_text_tab_scrolls_text() {
        use ratatui::layout::Rect;
        // Text tabs have no option rows: the wheel always scrolls the text.
        let mut d = QuestionDialog::new();
        d.show_questions(vec![QuestionItem {
            id: "t1".to_string(),
            question: "explique ".repeat(80),
            question_type: QuestionType::Text,
            purpose: None,
            options: None,
            required: true,
            recommended: None,
        }]);
        let area = Rect::new(0, 0, 80, 12);
        let option_y = test_option_y(&d, area);
        assert!(d.handle_wheel(option_y, area, true));
        assert_eq!(d.text_scroll, super::SCROLL_STEP);
    }
}
