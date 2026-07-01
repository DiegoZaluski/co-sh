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

    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        agent_colors: &AgentColors,
        unique_agents: &[String],
    ) {
        let input_area = Rect::new(area.x, area.y, area.width, 4);
        let cap_area = Rect::new(area.x, area.y + 4, area.width, 1);
        let footer_y = area.y + 5;

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

        // Left border only (no background fill) — creates the ┃ bar
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

        // Background fill starts at x+1 to leave a 1‑col gap from the border
        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_element.into()));
        let bg_area = Rect::new(input_area.x + 1, input_area.y, input_area.width.saturating_sub(1), input_area.height);
        bg_box.render_self(buf, bg_area);

        // Input text row
        let placeholder = "Ask anything... \"fix the linter\"";
        let display = if self.input.is_empty() {
            placeholder
        } else {
            &self.input
        };
        let input_style = if self.input.is_empty() {
            Style::default().fg(rgba_color(theme.text_muted))
        } else {
            Style::default().fg(rgba_color(theme.text))
        };
        let text_w = input_area.width.saturating_sub(5);
        draw_text_line(buf, display, input_area.x + 3, input_area.y + 1, text_w, input_style);

        // Agent label row
        let agent_label = capitalize(&agent_name);
        let label_style = Style::default().fg(rgba_color(agent_color));
        draw_text_line(buf, &agent_label, input_area.x + 3, input_area.y + 3, text_w, label_style);

        // Cap line: left corner ╹ + bottom border ▀
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

        // Footer — "esc interrupt"
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
