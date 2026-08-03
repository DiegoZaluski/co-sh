use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tools::question::types::{AnswerItem, QuestionItem, QuestionType};
use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use super::super::super::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

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

/// Per-question state tracked by the dialog.
#[derive(Debug, Clone)]
struct QuestionState {
    /// For SingleChoice/YesNo: which option index is selected (None = nothing selected)
    single_selection: Option<usize>,
    /// For `MultiChoice`: which option indices are checked
    multi_selection: Vec<usize>,
    /// For Text: the typed text input
    text_input: String,
    /// Whether this question has been "answered" (user pressed Enter on it)
    answered: bool,
}

impl QuestionState {
    const fn new() -> Self {
        Self {
            single_selection: None,
            multi_selection: Vec::new(),
            text_input: String::new(),
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
    /// Whether the user is typing text input (for Text-type questions).
    text_mode: bool,
}

impl QuestionDialog {
    pub const fn new() -> Self {
        Self {
            visible: false,
            submitted: false,
            questions: Vec::new(),
            state: Vec::new(),
            current_tab: 0,
            selected_row: 0,
            text_mode: false,
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
        self.text_mode = false;
        self.submitted = false;
        self.visible = true;
    }

    /// Build the answer items from the current dialog state.
    #[must_use]
    pub fn build_answers(&self) -> Vec<AnswerItem> {
        self.questions
            .iter()
            .zip(self.state.iter())
            .map(|(q, s)| {
                let (answer, selected) = match q.question_type {
                    QuestionType::Text => (Some(s.text_input.clone()), None),
                    QuestionType::SingleChoice => {
                        let sel = s
                            .single_selection
                            .and_then(|i| q.options.as_ref()?.get(i).cloned());
                        (None, sel.map(|s| vec![s]))
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
            QuestionType::SingleChoice | QuestionType::MultiChoice => {
                q.options.as_ref().map_or(0, std::vec::Vec::len)
            }
        }
    }

    /// Handle key events. Returns true if the key was consumed.
    pub fn handle_key(&mut self, key: crossterm::event::KeyCode) -> bool {
        if !self.visible {
            return false;
        }

        let tab_count = self.tab_count();
        let is_confirm = self.is_confirm();

        // Tab navigation always works
        match key {
            KeyCode::Left | KeyCode::Char('h') => {
                if tab_count > 1 {
                    self.current_tab = if self.current_tab == 0 {
                        tab_count - 1
                    } else {
                        self.current_tab - 1
                    };
                    self.selected_row = 0;
                    self.text_mode = false;
                }
                return true;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                if tab_count > 1 {
                    self.current_tab = (self.current_tab + 1) % tab_count;
                    self.selected_row = 0;
                    self.text_mode = false;
                }
                return true;
            }
            _ => {}
        }

        // Text mode: character keys go into the text input
        if self.text_mode
            && !is_confirm
            && let Some(q) = self.questions.get(self.current_tab)
            && q.question_type == QuestionType::Text
        {
            match key {
                KeyCode::Char(ch) => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.text_input.push(ch);
                    }
                    return true;
                }
                KeyCode::Backspace => {
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.text_input.pop();
                    }
                    return true;
                }
                KeyCode::Enter => {
                    self.text_mode = false;
                    if let Some(s) = self.state.get_mut(self.current_tab) {
                        s.answered = true;
                    }
                    // Move to next tab
                    if tab_count > 1 {
                        self.current_tab = (self.current_tab + 1) % tab_count;
                        self.selected_row = 0;
                    }
                    return true;
                }
                KeyCode::Esc => {
                    self.text_mode = false;
                    return true;
                }
                _ => {}
            }
        }

        // Options navigation (non-confirm tabs)
        if is_confirm {
            // Confirm tab
            match key {
                KeyCode::Enter => {
                    self.submitted = true;
                    return true;
                }
                KeyCode::Esc => {
                    self.visible = false;
                    return true;
                }
                _ => {}
            }
        } else {
            // Esc dismisses the dialog (like OpenCode)
            if key == KeyCode::Esc {
                self.visible = false;
                return true;
            }

            let count = self.row_count(self.current_tab);

            match key {
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
                // Number keys (1-9) to select and activate options directly (like OpenCode)
                // For Text questions, '1' enters text input mode
                ch @ (KeyCode::Char('1')
                | KeyCode::Char('2')
                | KeyCode::Char('3')
                | KeyCode::Char('4')
                | KeyCode::Char('5')
                | KeyCode::Char('6')
                | KeyCode::Char('7')
                | KeyCode::Char('8')
                | KeyCode::Char('9')) => {
                    if let KeyCode::Char(c) = ch {
                        let num = (c as usize) - ('1' as usize);
                        if num < count {
                            self.selected_row = num;
                            // For Text questions, '1' enters text mode (like OpenCode)
                            if let Some(q) = self.questions.get(self.current_tab)
                                && matches!(q.question_type, QuestionType::Text)
                            {
                                self.text_mode = true;
                                return true;
                            }
                            // Auto-select the option (like OpenCode's moveTo + selectOption)
                            return self.handle_key(KeyCode::Enter);
                        }
                        // num >= count: auto-enter text mode and type for Text questions
                        // (catch-all won't run since this pattern matched first)
                        if let Some(q) = self.questions.get(self.current_tab)
                            && q.question_type == QuestionType::Text
                        {
                            self.text_mode = true;
                            if let Some(s) = self.state.get_mut(self.current_tab) {
                                s.text_input.push(c);
                            }
                            return true;
                        }
                        // Always consume number keys to prevent leaks to the prompt
                        return true;
                    }
                }
                KeyCode::Enter => {
                    if let Some(q) = self.questions.get(self.current_tab) {
                        match q.question_type {
                            QuestionType::Text => {
                                // Enter text input mode
                                self.text_mode = true;
                                return true;
                            }
                            QuestionType::SingleChoice | QuestionType::YesNo => {
                                if let Some(s) = self.state.get_mut(self.current_tab) {
                                    s.single_selection = Some(self.selected_row);
                                    s.answered = true;
                                }
                                // Move to next tab
                                if tab_count > 1 {
                                    self.current_tab = (self.current_tab + 1) % tab_count;
                                    self.selected_row = 0;
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
                _ => {
                    // For Text questions, any printable character auto-enters text mode
                    // (like OpenCode where you can type directly without pressing Enter first)
                    if let KeyCode::Char(ch) = key
                        && let Some(q) = self.questions.get(self.current_tab)
                        && q.question_type == QuestionType::Text
                    {
                        self.text_mode = true;
                        if let Some(s) = self.state.get_mut(self.current_tab) {
                            s.text_input.push(ch);
                        }
                        return true;
                    }
                }
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
        let inner_x = area.x + 3;
        let _inner_w = area.width.saturating_sub(5);
        let footer_y = area.y + height - 1;

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
                        self.text_mode = false;
                        return true;
                    }
                    tab_x += label_len + 1;
                }
                // Confirm tab
                let confirm_label = " Confirm ";
                if x >= tab_x && x < tab_x + confirm_label.len() as u16 {
                    self.current_tab = self.questions.len();
                    self.selected_row = 0;
                    self.text_mode = false;
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
            let y_pos = area.y + 1 + u16::from(tab_count > 1) + 1; // after tabs + separator

            // Skip question text line
            // y_pos + 1 for question text
            // +1 if purpose is shown
            let mut option_y = y_pos + 1;
            if let Some(q) = self.questions.get(self.current_tab) {
                if q.purpose.is_some() {
                    option_y += 1;
                }

                let row_count = self.row_count(self.current_tab);
                // Check if click is within the option rows
                if y_click >= option_y && y_click < option_y + row_count as u16 {
                    let row = (y_click - option_y) as usize;
                    if row < row_count {
                        self.selected_row = row;
                        match q.question_type {
                            QuestionType::Text => {
                                self.text_mode = true;
                                return true;
                            }
                            QuestionType::SingleChoice | QuestionType::YesNo => {
                                if let Some(s) = self.state.get_mut(self.current_tab) {
                                    s.single_selection = Some(row);
                                    s.answered = true;
                                }
                                // Move to next tab
                                if tab_count > 1 {
                                    self.current_tab = (self.current_tab + 1) % tab_count;
                                    self.selected_row = 0;
                                }
                                return true;
                            }
                            QuestionType::MultiChoice => {
                                if let Some(s) = self.state.get_mut(self.current_tab) {
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
    pub fn required_height(&self, max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }

        let padding_vertical = 5u16;
        let footer = 1u16;

        if self.is_confirm() {
            // Header + each question summary
            padding_vertical + 1 + self.questions.len() as u16 + footer
        } else if let Some(q) = self.questions.get(self.current_tab) {
            let n_rows = self.row_count(self.current_tab) as u16;
            let tabs = u16::from(self.questions.len() > 1);
            let question_text_lines = self
                .wrap_lines(&q.question, max_width.saturating_sub(6))
                .max(1);
            padding_vertical + question_text_lines + n_rows + tabs + footer
        } else {
            padding_vertical + footer
        }
    }

    /// Estimate how many lines a text would wrap to at a given width.
    #[allow(clippy::unused_self)]
    fn wrap_lines(&self, text: &str, width: u16) -> u16 {
        if width < 10 {
            return text.len().max(1) as u16; // degenerate case
        }
        let chars_per_line = width as usize;
        let char_count = text.chars().count();
        if char_count == 0 {
            return 1;
        }
        char_count.div_ceil(chars_per_line).max(1) as u16
    }

    /// Render the question prompt inline inside the given area.
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let is_confirm = self.is_confirm();
        let tab_count = self.questions.len();
        let height = self.required_height(area.width).min(area.height);

        let inner_area = Rect::new(area.x, area.y, area.width, height);

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
        }

        // --- Separator ---
        y_pos += 1;

        if is_confirm {
            // --- Review screen ---
            let review_style = Style::default().fg(rgba_color(theme.text));
            draw_text_line(
                buf,
                "Review your answers:",
                inner_x,
                y_pos,
                inner_w,
                review_style,
            );
            y_pos += 1;

            for (i, q) in self.questions.iter().enumerate() {
                let display = self.question_summary(i);
                let label = format!(" {}: ", q.id);
                let label_style = Style::default().fg(rgba_color(theme.text_muted));
                draw_text_line(buf, &label, inner_x, y_pos, inner_w, label_style);
                let val_x = inner_x + label.len() as u16;
                let val_style = if self.state.get(i).is_some_and(|s| s.answered) {
                    Style::default().fg(rgba_color(theme.text))
                } else {
                    Style::default().fg(rgba_color(theme.error))
                };
                draw_text_line(
                    buf,
                    &display,
                    val_x,
                    y_pos,
                    inner_w.saturating_sub(val_x - inner_x),
                    val_style,
                );
                // Purpose hint
                if let Some(ref purpose) = q.purpose
                    && y_pos + 1 < area.y + height - 1
                {
                    y_pos += 1;
                    let purpose_text = format!("  └ {purpose}");
                    draw_text_line(
                        buf,
                        &purpose_text,
                        inner_x,
                        y_pos,
                        inner_w,
                        Style::default().fg(rgba_color(theme.text_muted)),
                    );
                }
                y_pos += 1;
            }
        } else if let Some(q) = self.questions.get(self.current_tab) {
            // --- Question text ---
            let qstyle = Style::default().fg(rgba_color(theme.text));
            draw_text_line(buf, &q.question, inner_x, y_pos, inner_w, qstyle);
            y_pos += 1;

            // Purpose hint
            if let Some(ref purpose) = q.purpose {
                let purpose_text = format!("  ({purpose})");
                let purpose_style = Style::default().fg(rgba_color(theme.text_muted));
                draw_text_line(buf, &purpose_text, inner_x, y_pos, inner_w, purpose_style);
                y_pos += 1;
            }

            match q.question_type {
                QuestionType::Text => {
                    // Text input field — no explicit background (inherits from box's background_panel)
                    let state = self.state.get(self.current_tab);
                    let input_text = state.map_or(String::new(), |s| {
                        if s.text_input.is_empty() {
                            "Type your answer...".to_string()
                        } else {
                            s.text_input.clone()
                        }
                    });
                    let input_fg = if self.text_mode {
                        rgba_color(theme.text)
                    } else {
                        rgba_color(theme.text_muted)
                    };
                    draw_text_line(
                        buf,
                        &format!("  {input_text}"),
                        inner_x,
                        y_pos,
                        inner_w,
                        Style::default()
                            .fg(input_fg)
                            .bg(rgba_color(theme.background_panel)),
                    );
                }

                QuestionType::YesNo => {
                    for (i, opt) in ["Yes", "No"].iter().enumerate() {
                        let is_active = i == self.selected_row;

                        // Active row background
                        if is_active {
                            let bg_color = rgba_color(theme.background_element);
                            for cx in inner_x..inner_x + inner_w {
                                if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                                    cell.set_char(' ');
                                    cell.set_style(Style::default().bg(bg_color));
                                }
                            }
                        }

                        // Radio indicator
                        let is_selected = self
                            .state
                            .get(self.current_tab)
                            .is_some_and(|s| s.single_selection == Some(i));
                        let indicator = if is_selected { "◉ " } else { "○ " };
                        let indicator_fg = if is_selected {
                            theme.accent
                        } else if is_active {
                            theme.secondary
                        } else {
                            theme.text_muted
                        };
                        draw_text_line(
                            buf,
                            indicator,
                            inner_x,
                            y_pos,
                            inner_w,
                            Style::default().fg(rgba_color(indicator_fg)),
                        );

                        // Option label
                        let opt_fg = if is_active {
                            theme.secondary
                        } else {
                            theme.text
                        };
                        draw_text_line(
                            buf,
                            opt,
                            inner_x + 3,
                            y_pos,
                            inner_w.saturating_sub(3),
                            Style::default().fg(rgba_color(opt_fg)),
                        );

                        y_pos += 1;
                    }
                }

                QuestionType::SingleChoice => {
                    if let Some(ref opts) = q.options {
                        for (i, opt) in opts.iter().enumerate() {
                            let is_active = i == self.selected_row;

                            // Active row background
                            if is_active {
                                let bg_color = rgba_color(theme.background_element);
                                for cx in inner_x..inner_x + inner_w {
                                    if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                                        cell.set_char(' ');
                                        cell.set_style(Style::default().bg(bg_color));
                                    }
                                }
                            }

                            // Radio indicator
                            let is_selected = self
                                .state
                                .get(self.current_tab)
                                .is_some_and(|s| s.single_selection == Some(i));
                            let indicator = if is_selected { "◉ " } else { "○ " };
                            let indicator_fg = if is_selected {
                                theme.accent
                            } else if is_active {
                                theme.secondary
                            } else {
                                theme.text_muted
                            };
                            draw_text_line(
                                buf,
                                indicator,
                                inner_x,
                                y_pos,
                                inner_w,
                                Style::default().fg(rgba_color(indicator_fg)),
                            );

                            // Option label
                            let opt_fg = if is_active {
                                theme.secondary
                            } else {
                                theme.text
                            };
                            draw_text_line(
                                buf,
                                opt,
                                inner_x + 3,
                                y_pos,
                                inner_w.saturating_sub(3),
                                Style::default().fg(rgba_color(opt_fg)),
                            );

                            y_pos += 1;
                        }
                    }
                }

                QuestionType::MultiChoice => {
                    if let Some(ref opts) = q.options {
                        for (i, opt) in opts.iter().enumerate() {
                            let is_active = i == self.selected_row;

                            // Active row background
                            if is_active {
                                let bg_color = rgba_color(theme.background_element);
                                for cx in inner_x..inner_x + inner_w {
                                    if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                                        cell.set_char(' ');
                                        cell.set_style(Style::default().bg(bg_color));
                                    }
                                }
                            }

                            // Checkbox indicator
                            let is_checked = self
                                .state
                                .get(self.current_tab)
                                .is_some_and(|s| s.multi_selection.contains(&i));
                            let indicator = if is_checked { "☑ " } else { "☐ " };
                            let indicator_fg = if is_checked {
                                theme.accent
                            } else if is_active {
                                theme.secondary
                            } else {
                                theme.text_muted
                            };
                            draw_text_line(
                                buf,
                                indicator,
                                inner_x,
                                y_pos,
                                inner_w,
                                Style::default().fg(rgba_color(indicator_fg)),
                            );

                            // Option label
                            let opt_fg = if is_active {
                                theme.secondary
                            } else {
                                theme.text
                            };
                            draw_text_line(
                                buf,
                                opt,
                                inner_x + 3,
                                y_pos,
                                inner_w.saturating_sub(3),
                                Style::default().fg(rgba_color(opt_fg)),
                            );

                            y_pos += 1;
                        }
                    }
                }
            }
        }

        // --- Footer (keyboard hints, inherits background_panel from the box) ---
        let footer_y = inner_area.bottom().saturating_sub(2);
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
        // Suppress "value assigned to `fx` is never read" warning
        let _ = fx;
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
            QuestionType::SingleChoice => s
                .single_selection
                .and_then(|i| q.options.as_ref()?.get(i).cloned())
                .unwrap_or_else(|| "(not answered)".into()),
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
