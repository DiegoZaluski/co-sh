use std::time::SystemTime;

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

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
    pub history: Vec<String>,
    pub history_index: i32,
    pub selected_agent_index: usize,
    pub is_focused: bool,
    pub terminal_focused: bool,
    pub last_input_at: SystemTime,
    pub blink_start: SystemTime,
}

impl PromptView {
    pub fn new() -> Self {
        PromptView {
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_index: -1,
            selected_agent_index: 0,
            is_focused: true,
            terminal_focused: true,
            last_input_at: SystemTime::now(),
            blink_start: SystemTime::now(),
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

    pub fn next_agent(&mut self, num_agents: usize) {
        if num_agents == 0 {
            return;
        }
        self.selected_agent_index = (self.selected_agent_index + 1) % num_agents;
    }

    pub fn note_activity(&mut self) {
        self.last_input_at = SystemTime::now();
        self.blink_start = SystemTime::now();
    }

    pub fn prev_agent(&mut self, num_agents: usize) {
        if num_agents == 0 {
            return;
        }
        self.selected_agent_index = if self.selected_agent_index == 0 {
            num_agents - 1
        } else {
            self.selected_agent_index - 1
        };
    }

    pub fn required_height(&self, area_width: u16) -> u16 {
        let text_w = area_width.saturating_sub(5) as usize;
        let lines = if self.input.is_empty() || text_w == 0 {
            1
        } else {
            let n = self.input.chars().count();
            n.div_ceil(text_w)
        };
        BASE_H + lines as u16 + AGENT_H + CAP_H + FOOTER_H
    }

    fn wrapped_lines(input: &str, max_w: usize) -> Vec<&str> {
        if input.is_empty() || max_w == 0 {
            return vec![""];
        }
        let mut lines = Vec::new();
        let mut s = input;
        while !s.is_empty() {
            let line_len = s.chars().take(max_w).count();
            let split = s.char_indices().nth(line_len).map_or(s.len(), |(i, _)| i);
            lines.push(&s[..split]);
            s = &s[split..];
        }
        lines
    }

    /// Render the prompt and draw a blinking cursor if focused.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        _state: &AppState,
        theme: &Theme,
        agent_colors: &AgentColors,
        unique_agents: &[String],
        now: SystemTime,
        model_name: &str,
    ) {
        let text_w = area.width.saturating_sub(5) as usize;
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

        let agent_name = if unique_agents.is_empty() {
            "build".to_string()
        } else {
            let idx = self
                .selected_agent_index
                .min(unique_agents.len().saturating_sub(1));
            unique_agents[idx].clone()
        };

        let agent_color = agent_colors.get(&agent_name, unique_agents);

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

        let agent_label = capitalize(&agent_name);
        let label_style = Style::default().fg(rgba_color(agent_color));
        let label_y = input_area.y + BASE_H + n;
        draw_text_line(buf, &agent_label, x_off, label_y, max_line_w, label_style);

        // Model name on the right side of the same line
        if !model_name.is_empty() {
            let model_text = format!(" {}", model_name);
            let model_x = input_area.right().saturating_sub(model_text.len() as u16);
            let muted_style = Style::default().fg(rgba_color(theme.text_muted));
            draw_text_line(buf, &model_text, model_x, label_y, model_text.len() as u16, muted_style);
        }

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

        // Draw cursor if focused
        if self.is_focused {
            let text_w_val = text_w.max(1);
            let cursor_pos_for_calc = if display_placeholder {
                0
            } else {
                self.cursor_pos.min(self.input.chars().count())
            };
            let cursor_line_idx = cursor_pos_for_calc / text_w_val;
            let cursor_col_idx = cursor_pos_for_calc % text_w_val;

            let cursor_y = text_start + cursor_line_idx as u16;
            if cursor_y < input_area.bottom() && cursor_line_idx < display_lines.len() {
                let cursor_x = x_off + cursor_col_idx as u16;
                if cursor_x < input_area.right()
                    && let Some(cell) = buf.cell_mut((cursor_x, cursor_y))
                {
                    if self.terminal_focused {
                        // Steady cursor while typing; blink after 500ms idle
                        let idle_ms = now
                            .duration_since(self.last_input_at)
                            .map_or(0, |d| d.as_millis());
                        let show = if idle_ms < 500 {
                            true
                        } else {
                            let elapsed_ms = now
                                .duration_since(self.blink_start)
                                .map_or(0, |d| d.as_millis() % 1000);
                            elapsed_ms < 500
                        };
                        if show {
                            // ON: transparent cursor (invert colors)
                            cell.set_style(
                                Style::default()
                                    .fg(rgba_color(theme.background))
                                    .bg(rgba_color(theme.text)),
                            );
                        } else {
                            // OFF: dimmed visible state (not invisible)
                            cell.set_style(
                                Style::default()
                                    .fg(rgba_color(theme.text_muted))
                                    .bg(rgba_color(theme.background)),
                            );
                        }
                    } else {
                        // Terminal unfocused: transparent dark
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

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
