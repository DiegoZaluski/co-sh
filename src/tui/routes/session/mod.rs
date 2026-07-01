pub mod footer;
pub mod permission;
pub mod question;
pub mod sidebar;
pub mod subagent_footer;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::styled_text::string_to_styled_text;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use crate::state::AppState;
use crate::theme::Theme;
use crate::types::*;

fn left_border_chars() -> BorderCharacters {
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

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    max_w: u16,
    style: Style,
) {
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

fn draw_text_block(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    style: Style,
) {
    let bottom = y + max_h;
    for (i, line) in text.lines().enumerate() {
        let ly = y + i as u16;
        if ly >= bottom {
            break;
        }
        draw_text_line(buf, line, x, ly, max_w, style);
    }
}

pub struct SessionView {
    pub scroll_y: i32,
}

impl SessionView {
    pub fn new() -> Self {
        SessionView { scroll_y: 0 }
    }

    fn render_user_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_color: RGBA,
    ) {
        let text_content = msg
            .parts
            .iter()
            .filter_map(|p| {
                if let Part::Text(t) = p
                    && !t.synthetic {
                        return Some(t.text.as_str());
                    }
                None
            })
            .collect::<Vec<&str>>()
            .join("\n");

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(agent_color.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(left_border_chars());
        border_box.render_self(buf, area);

        let x_off = area.x + 3;
        let text_y = area.y + 1;

        for (i, line) in text_content.lines().enumerate() {
            let y = text_y + i as u16;
            if y >= area.bottom() {
                break;
            }
            let mut text_r = TextRenderable::new(Some(string_to_styled_text(line)));
            text_r.set_fg(theme.text);
            text_r.set_bg(theme.background_panel);
            let line_area = Rect::new(x_off, y, area.width.saturating_sub(6), 1);
            text_r.render_self(buf, line_area);
        }
    }

    fn render_assistant_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_colors: &AgentColors,
        is_last: bool,
        unique_agents: &[String],
    ) {
        let text_content = msg
            .parts
            .iter()
            .filter_map(|p| {
                if let Part::Text(t) = p
                    && !t.synthetic {
                        return Some(t.text.as_str());
                    }
                None
            })
            .collect::<Vec<&str>>()
            .join("\n");

        let x_off = area.x + 3;
        let text_y = area.y;

        for (i, line) in text_content.lines().enumerate() {
            let y = text_y + i as u16;
            if y >= area.bottom() {
                break;
            }
            let mut text_r = TextRenderable::new(Some(string_to_styled_text(line)));
            text_r.set_fg(theme.text);
            text_r.set_bg(theme.background);
            let line_area = Rect::new(x_off, y, area.width.saturating_sub(6), 1);
            text_r.render_self(buf, line_area);
        }

        if is_last {
            let model_name = msg.model.as_deref().unwrap_or("assistant");
            let agent = msg.agent.as_deref().unwrap_or("default");
            let agent_color = agent_colors.get(agent, unique_agents);

            let meta_y = text_y + text_content.lines().count() as u16 + 1;
            if meta_y < area.bottom() {
                let muted_style = Style::default().fg(rgba_color(theme.text_muted));
                let icon_style = Style::default()
                    .fg(rgba_color(agent_color));

                if let Some(cell) = buf.cell_mut((x_off, meta_y)) {
                    cell.set_char('\u{25a3}');
                    cell.set_style(icon_style);
                }
                let rest = format!(" chat \u{b7} {}", model_name);
                let text_w = area.width.saturating_sub(6);
                draw_text_line(buf, &rest, x_off + 1, meta_y, text_w.saturating_sub(1), muted_style);
            }
        }
    }

    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
    ) -> i32 {
        let session = match state.current_session() {
            Some(s) => s,
            None => return 0,
        };

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );

        let unique_agents = state.unique_agents();
        let agent_colors = AgentColors::from_theme(theme);

        let mut total_height: i32 = 0;
        for (idx, msg) in session.messages.iter().enumerate() {
            let text = msg
                .parts
                .iter()
                .filter_map(|p| {
                    if let Part::Text(t) = p
                        && !t.synthetic {
                            return Some(t.text.as_str());
                        }
                    None
                })
                .collect::<Vec<&str>>()
                .join("\n");

            let lines = text.lines().count().max(1) as i32;

            let mut msg_h = lines + 2;
            if idx == session.messages.len() - 1 && msg.role == MessageRole::Assistant {
                msg_h += 2;
            }

            let gap = if idx > 0 { 1 } else { 0 };
            total_height += gap + msg_h;
        }

        let visible_height = inner_area.height as i32;
        let max_scroll = (total_height - visible_height).max(0);
        self.scroll_y = self.scroll_y.clamp(0, max_scroll);

        let mut y = inner_area.y as i32 - self.scroll_y;

        for (idx, msg) in session.messages.iter().enumerate() {
            let text = msg
                .parts
                .iter()
                .filter_map(|p| {
                    if let Part::Text(t) = p
                        && !t.synthetic {
                            return Some(t.text.as_str());
                        }
                    None
                })
                .collect::<Vec<&str>>()
                .join("\n");

            let lines = text.lines().count().max(1) as i32;

            if idx > 0 {
                y += 1;
            }

            let mut msg_h = lines + 2;
            let is_last = idx == session.messages.len() - 1;
            if is_last && msg.role == MessageRole::Assistant {
                msg_h += 1;
            }

            let msg_y = y.max(inner_area.y as i32 - 1) as u16;
            let visible_bottom = inner_area.bottom();

            if msg_y < visible_bottom {
                let msg_area = Rect::new(
                    inner_area.x,
                    msg_y,
                    inner_area.width,
                    msg_h.min((visible_bottom - msg_y) as i32) as u16,
                );

                match msg.role {
                    MessageRole::User => {
                        let agent_name = msg.agent.as_deref().unwrap_or("default");
                        let agent_color = agent_colors.get(agent_name, &unique_agents);
                        Self::render_user_message(buf, msg_area, msg, theme, agent_color);
                    }
                    MessageRole::Assistant => {
                        Self::render_assistant_message(
                            buf,
                            msg_area,
                            msg,
                            theme,
                            &agent_colors,
                            is_last,
                            &unique_agents,
                        );
                    }
                }
            }

            y += msg_h;
        }

        total_height
    }
}
