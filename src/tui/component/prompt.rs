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
use crate::types::AgentColors;

const BASE_H: u16 = 2;
const AGENT_H: u16 = 1;
const CAP_H: u16 = 1;
const FOOTER_H: u16 = 1;
const PLACEHOLDER: &str = "Type a message...";

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn prompt_border_chars() -> BorderCharacters {
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
    pub history: Vec<String>,
    pub history_index: i32,
    pub is_focused: bool,
    pub cursor: Cursor,
    pub sel_start: Option<usize>,
    pub sel_end: Option<usize>,
}

impl PromptView {
    pub fn new() -> Self {
        PromptView {
            input: String::new(),
            cursor_pos: 0,
            input_text_width: Cell::new(0),
            history: Vec::new(),
            history_index: -1,
            is_focused: true,
            cursor: Cursor::new(),
            sel_start: None,
            sel_end: None,
        }
    }

    pub fn focus(&mut self) {
        self.is_focused = true;
    }

    pub fn blur(&mut self) {
        self.is_focused = false;
    }

    pub fn send_message(&mut self) -> String {
        let msg = self.input.clone();
        if !msg.is_empty() {
            self.history.push(msg.clone());
        }
        self.input.clear();
        self.cursor_pos = 0;
        self.history_index = -1;
        msg
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index == -1 {
            self.history_index = i32::try_from(self.history.len()).unwrap_or(i32::MAX) - 1;
        } else if self.history_index > 0 {
            self.history_index -= 1;
        }
        self.input = self.history[usize::try_from(self.history_index).unwrap_or(0)].clone();
        self.cursor_pos = self.input.len();
    }

    pub fn history_down(&mut self) {
        if self.history_index == -1 {
            return;
        }
        self.history_index += 1;
        if self.history_index >= i32::try_from(self.history.len()).unwrap_or(i32::MAX) {
            self.history_index = -1;
            self.input.clear();
        } else {
            self.input = self.history[usize::try_from(self.history_index).unwrap_or(0)].clone();
        }
        self.cursor_pos = self.input.len();
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
                self.input[start..end].to_string()
            }
            _ => String::new(),
        }
    }

    pub fn clear_selection(&mut self) {
        self.sel_start = None;
        self.sel_end = None;
    }

    /// Delete the word (or run of whitespace then word) immediately before the cursor.
    /// Behaves like Ctrl+Backspace in most terminals/editors.
    pub fn delete_word_before_cursor(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }

        let pos = self.cursor_pos.min(self.input.len());
        let mut i = pos;

        // 1. Skip any whitespace immediately before the cursor.
        while i > 0 {
            let prev = self.input.floor_char_boundary(i - 1);
            if prev >= i {
                break;
            }
            let ch = self.input[prev..i].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                i = prev;
            } else {
                break;
            }
        }

        // 2. Skip the word (non-whitespace) characters.
        while i > 0 {
            let prev = self.input.floor_char_boundary(i - 1);
            if prev >= i {
                break;
            }
            let ch = self.input[prev..i].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                i = prev;
            } else {
                break;
            }
        }

        if i < self.cursor_pos {
            self.note_activity();
            self.input.drain(i..self.cursor_pos);
            self.cursor_pos = i;
        }
    }

    pub fn cursor_up(&mut self, text_w: usize) {
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
            let visual_chars = visual_lines * cols;
            if char_pos < acc + visual_chars {
                let offset = char_pos - acc;
                let visual_line = offset / cols;
                if visual_line == 0 {
                    if li == 0 {
                        return; // already top
                    }
                    // move to end of previous logical line
                    let prev = byte_off.saturating_sub(1); // position of '\n'
                    self.cursor_pos = prev;
                    return;
                }
                let visual_col = offset % cols;
                let target = (visual_line - 1) * cols + visual_col.min(cols - 1);
                let target_byte = line
                    .char_indices()
                    .nth(target)
                    .map_or(line.len(), |(i, _)| i);
                self.cursor_pos = byte_off + target_byte;
                return;
            }
            acc += visual_chars;
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
            let visual_chars = visual_lines * cols;
            if char_pos < acc + visual_chars {
                let offset = char_pos - acc;
                let visual_line = offset / cols;
                let visual_col = offset % cols;
                if visual_line + 1 >= visual_lines {
                    // move to next logical line
                    if li + 1 >= lines.len() {
                        return;
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
                // move down one visual line within same logical line
                let target = (visual_line + 1) * cols + visual_col.min(cols - 1);
                let target_byte = line
                    .char_indices()
                    .nth(target)
                    .map_or(line.len(), |(i, _)| i);
                self.cursor_pos = byte_off + target_byte;
                return;
            }
            acc += visual_chars;
            byte_off += line.len() + 1;
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
        let mut byte_off = 0usize;
        for line in display_lines.iter().take(visual_line) {
            byte_off += line.len();
        }
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

        for (i, line) in display_lines.iter().enumerate() {
            let ly = text_start + i as u16;
            if ly >= input_area.bottom() {
                break;
            }
            let style = if display_placeholder && i == 0 {
                Style::default().fg(rgba_color(theme.text_muted))
            } else {
                Style::default().fg(rgba_color(theme.text))
            };
            draw_text_line(buf, line, x_off, ly, max_line_w, style);
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
                if cursor_char < acc + n
                    || (cursor_char == acc + n && li + 1 >= display_lines.len())
                {
                    cursor_line_idx = li;
                    cursor_col_idx = cursor_char - acc;
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
