use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::state::AppState;
use crate::theme::Theme;
use crate::types::*;

const BASE_H: u16 = 2;
const AGENT_H: u16 = 1;
const CAP_H: u16 = 1;
const FOOTER_H: u16 = 1;

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
}

impl PromptView {
    pub fn new() -> Self {
        PromptView {
            input: String::new(),
            cursor_pos: 0,
        }
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
            let split = s
                .char_indices()
                .nth(line_len)
                .map(|(i, _)| i)
                .unwrap_or(s.len());
            lines.push(&s[..split]);
            s = &s[split..];
        }
        lines
    }

    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        agent_colors: &AgentColors,
        unique_agents: &[String],
    ) {
        let text_w = area.width.saturating_sub(5) as usize;
        let display_lines = if self.input.is_empty() {
            vec![""]
        } else {
            Self::wrapped_lines(&self.input, text_w)
        };
        let n = display_lines.len() as u16;

        let input_h = BASE_H + n;
        let input_area = Rect::new(area.x, area.y, area.width, input_h);
        let cap_y = area.y + input_h + AGENT_H;
        let cap_area = Rect::new(area.x, cap_y, area.width, CAP_H);
        let footer_y = cap_y + CAP_H;

        let agent_name = state
            .current_session()
            .and_then(|s| {
                s.messages
                    .iter()
                    .find(|m| m.role == MessageRole::User)
                    .and_then(|m| m.agent.clone())
            })
            .unwrap_or_else(|| "build".to_string());

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
        let input_style_base = if self.input.is_empty() {
            Style::default().fg(rgba_color(theme.text_muted))
        } else {
            Style::default().fg(rgba_color(theme.text))
        };
        let max_line_w = input_area.width.saturating_sub(5) as u16;

        for (i, line) in display_lines.iter().enumerate() {
            let ly = text_start + i as u16;
            if ly >= input_area.bottom() {
                break;
            }
            let style = if self.input.is_empty() && i == 0 {
                Style::default().fg(rgba_color(theme.text_muted))
            } else {
                input_style_base
            };
            draw_text_line(buf, line, x_off, ly, max_line_w, style);
        }

        let agent_label = capitalize(&agent_name);
        let label_style = Style::default().fg(rgba_color(agent_color));
        let label_y = input_area.bottom();
        if label_y < area.bottom() {
            draw_text_line(buf, &agent_label, x_off, label_y, max_line_w, label_style);
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
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
