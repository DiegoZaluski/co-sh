pub mod footer;
pub mod permission;
pub mod question;
pub mod sidebar;
pub mod subagent_footer;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::scroll_bar::{ScrollBarOrientation, ScrollBarRenderable};

use crate::config::TuiConfig;
use crate::state::AppState;
use crate::theme::Theme;
use crate::types::{AgentColors, FilePart, Message, MessageRole, Part, ReasoningPart, ToolStatus};
use crate::util::tool_render::{self, ToolRenderState};

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

fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn concealed_char(ch: char) -> char {
    if ch == ' ' { ' ' } else { '\u{2588}' }
}

fn conceal_text(text: &str) -> String {
    text.chars().map(concealed_char).collect()
}

pub struct SessionView {
    pub scroll_y: i32,
    pub tool_state: ToolRenderState,
}

impl SessionView {
    pub fn new() -> Self {
        SessionView {
            scroll_y: 0,
            tool_state: ToolRenderState::new(),
        }
    }

    fn render_file_badge(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        theme: &Theme,
        file: &FilePart,
    ) {
        let is_dir = file.mime == "application/x-directory";
        let tag = if is_dir { " Directory " } else { " File " };
        let label = format!("{tag}{}", file.filename);
        let fg = if is_dir { theme.secondary } else { theme.info };
        let style = Style::default().fg(rgba_color(fg));
        draw_text_line(buf, &label, x, y, max_w, style);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_reasoning(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        line_h: &mut u16,
        max_w: u16,
        part: &ReasoningPart,
        expanded: bool,
        theme: &Theme,
    ) {
        let header = if expanded { "- " } else { "+ " };
        let title = format!("{header}Thought");
        let header_style = Style::default().fg(rgba_color(theme.warning));
        draw_text_line(buf, &title, x, y, max_w, header_style);
        *line_h = 1;

        if expanded && !part.text.is_empty() {
            let md_h = part.text.lines().count().min(10) as u16 + 1;
            let md_area = Rect::new(x + 2, y + 1, max_w.saturating_sub(2), md_h);
            let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(
                part.text.clone(),
            ));
            md.set_fg(Some(ColorInput::RGBA(theme.text_muted)));
            md.set_bg(Some(ColorInput::RGBA(theme.background)));
            md.render_self(buf, md_area);
            *line_h = 1 + md_h;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_parts(
        buf: &mut Buffer,
        x: u16,
        y_start: u16,
        max_w: u16,
        max_h: u16,
        parts: &[Part],
        role: &MessageRole,
        theme: &Theme,
        tool_state: &ToolRenderState,
        config: &TuiConfig,
    ) -> u16 {
        let mut y = y_start;
        let bottom = y_start + max_h;
        let fg_color = if config.conceal {
            theme.text_muted
        } else {
            theme.text
        };

        for part in parts {
            if y >= bottom {
                break;
            }

            match part {
                Part::Text(t) if !t.synthetic && *role == MessageRole::Assistant => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        t.text.clone()
                    };
                    let lines = content.lines().count().max(1) as u16;
                    let h = lines.min(bottom - y);
                    let area = Rect::new(x, y, max_w, h);
                    let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                        Some(content),
                    );
                    md.set_fg(Some(ColorInput::RGBA(fg_color)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                    md.render_self(buf, area);
                    y += h;
                }
                Part::Text(t) if !t.synthetic => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        t.text.clone()
                    };
                    let lines = content.lines().count().max(1) as u16;
                    let h = lines.min(bottom - y).max(1);
                    if h > 0 {
                        let text_style = Style::default().fg(rgba_color(fg_color));
                        draw_text_line(buf, &content, x, y, max_w, text_style);
                    }
                    y += h;
                }
                Part::Tool(tool) => {
                    if !config.show_tool_details && matches!(tool.status, ToolStatus::Completed) {
                        y += 1;
                        continue;
                    }
                    if !config.show_generic_tool_output
                        && tool_render::tool_display(&tool.tool) == "generic"
                    {
                        y += 1;
                        continue;
                    }
                    let mut line_h = 0u16;
                    tool_render::dispatch_tool(
                        buf,
                        x,
                        y,
                        &mut line_h,
                        max_w,
                        tool,
                        tool_state,
                        theme,
                    );
                    y += line_h.max(1);
                }
                Part::Reasoning(r) => {
                    let expanded = config.thinking_mode
                        || tool_state.is_expanded(&r.text[..r.text.len().min(32)]);
                    let mut line_h = 0u16;
                    Self::render_reasoning(buf, x, y, &mut line_h, max_w, r, expanded, theme);
                    y += line_h.max(1);
                }
                Part::File(f) => {
                    Self::render_file_badge(buf, x, y, max_w, theme, f);
                    y += 1;
                }
                Part::Text(_) => {}
            }
        }

        y - y_start
    }

    fn estimate_part_height(part: &Part, _max_w: u16, config: &TuiConfig) -> u16 {
        match part {
            Part::Text(t) if !t.synthetic => t.text.lines().count().max(1) as u16,
            Part::Tool(t) => {
                if !config.show_tool_details && matches!(t.status, ToolStatus::Completed) {
                    return 1;
                }
                if !config.show_generic_tool_output
                    && tool_render::tool_display(&t.tool) == "generic"
                {
                    return 1;
                }
                let is_block = t.output.is_some()
                    && matches!(t.status, ToolStatus::Completed)
                    && !t.output.as_deref().unwrap_or("").trim().is_empty();
                if is_block {
                    let lines = t.output.as_deref().unwrap_or("").lines().count().max(1) as u16;
                    lines + 2
                } else {
                    1
                }
            }
            Part::Reasoning(r) => {
                if r.text.is_empty() {
                    1
                } else {
                    (r.text.lines().count().min(10) as u16) + 2
                }
            }
            Part::File(_) => 1,
            Part::Text(_) => 0,
        }
    }

    fn render_queued_badge(buf: &mut Buffer, x: u16, y: u16, theme: &Theme) {
        let badge_style = Style::default().fg(rgba_color(theme.warning));
        draw_text_line(buf, " [QUEUED]", x, y, 16, badge_style);
    }

    fn render_compaction_banner(buf: &mut Buffer, x: u16, y: u16, max_w: u16, theme: &Theme) {
        let line_style = Style::default().fg(rgba_color(theme.text_muted));
        let dash = '\u{2500}';
        let right = x + max_w;
        for cx in x..right {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(dash);
                cell.set_style(line_style);
            }
        }
        let label = " Compaction ";
        let label_x = x + max_w.saturating_sub(label.len() as u16 + 2);
        let label_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, label, label_x, y, label.len() as u16, label_style);
    }

    fn render_timestamp(buf: &mut Buffer, x: u16, y: u16, ts: u64, theme: &Theme) {
        let ts_str = format_timestamp(ts);
        let ts_style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text_line(buf, &format!(" [{ts_str}]"), x, y, 12, ts_style);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_user_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_color: RGBA,
        tool_state: &ToolRenderState,
        config: &TuiConfig,
        is_queued: bool,
        is_compacted: bool,
    ) {
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
        let max_w = area.width.saturating_sub(6);
        let inner_h = area.height.saturating_sub(1);

        if is_compacted {
            Self::render_compaction_banner(buf, x_off, area.y + 1, max_w, theme);
        }

        let inner_y = area.y + 1 + u16::from(is_compacted);
        let remaining = inner_h.saturating_sub(u16::from(is_compacted));

        Self::render_parts(
            buf,
            x_off,
            inner_y,
            max_w,
            remaining,
            &msg.parts,
            &MessageRole::User,
            theme,
            tool_state,
            config,
        );

        if is_queued {
            Self::render_queued_badge(buf, x_off, area.y, theme);
        }

        if config.show_timestamps {
            Self::render_timestamp(buf, x_off, area.y, msg.created_at, theme);
        }
    }

    #[allow(clippy::too_many_arguments, clippy::cast_sign_loss)]
    fn render_assistant_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_colors: &AgentColors,
        is_last: bool,
        unique_agents: &[String],
        tool_state: &ToolRenderState,
        config: &TuiConfig,
        is_queued: bool,
        is_compacted: bool,
    ) {
        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6);

        let banner_h = u16::from(is_compacted);
        let inner_y = area.y + banner_h;

        Self::render_parts(
            buf,
            x_off,
            inner_y,
            max_w,
            area.height.saturating_sub(banner_h),
            &msg.parts,
            &MessageRole::Assistant,
            theme,
            tool_state,
            config,
        );

        if is_compacted {
            Self::render_compaction_banner(buf, x_off, area.y, max_w, theme);
        }

        if is_queued {
            Self::render_queued_badge(buf, x_off, area.y, theme);
        }

        if config.show_timestamps {
            Self::render_timestamp(buf, x_off, area.y, msg.created_at, theme);
        }

        if is_last {
            let last_part_end = inner_y + area.height.saturating_sub(banner_h);
            let meta_y = last_part_end;
            if meta_y < area.bottom() {
                let model_name = msg.model.as_deref().unwrap_or("assistant");
                let agent = msg.agent.as_deref().unwrap_or("default");
                let agent_color = agent_colors.get(agent, unique_agents);

                let muted_style = Style::default().fg(rgba_color(theme.text_muted));
                let icon_style = Style::default().fg(rgba_color(agent_color));

                if let Some(cell) = buf.cell_mut((x_off, meta_y)) {
                    cell.set_char('\u{25a3}');
                    cell.set_style(icon_style);
                }
                let rest = format!(" chat \u{b7} {model_name}");
                draw_text_line(
                    buf,
                    &rest,
                    x_off + 1,
                    meta_y,
                    max_w.saturating_sub(1),
                    muted_style,
                );
            }
        }
    }

    #[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        config: &TuiConfig,
    ) -> i32 {
        let Some(session) = state.current_session() else {
            return 0;
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
        let max_w = inner_area.width.saturating_sub(6);

        let mut total_height: i32 = 0;
        for (idx, msg) in session.messages.iter().enumerate() {
            let gap = i32::from(idx > 0);
            let mut msg_h = 2i32;
            for part in &msg.parts {
                msg_h += i32::from(Self::estimate_part_height(part, max_w, config));
            }
            if idx == session.messages.len() - 1 && msg.role == MessageRole::Assistant {
                msg_h += 2;
            }
            total_height += gap + msg_h;
        }

        let visible_height = i32::from(inner_area.height);
        let max_scroll = (total_height - visible_height).max(0);
        self.scroll_y = self.scroll_y.clamp(0, max_scroll);

        if config.show_scrollbar {
            let scrollbar_area = Rect::new(
                inner_area.right().saturating_sub(1),
                inner_area.y,
                1,
                inner_area.height,
            );
            let mut scrollbar = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
            scrollbar.set_scroll_size(f64::from(total_height));
            scrollbar.set_viewport_size(f64::from(visible_height));
            scrollbar.set_scroll_position(f64::from(self.scroll_y));
            scrollbar.set_track_color(Some(theme.background.into()));
            scrollbar.set_thumb_color(Some(theme.text_muted.into()));
            scrollbar.render_self(buf, scrollbar_area);
        }

        let mut y = i32::from(inner_area.y) - self.scroll_y;

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let mut msg_h = 2i32;
            for part in &msg.parts {
                msg_h += i32::from(Self::estimate_part_height(part, max_w, config));
            }
            let is_last = idx == session.messages.len() - 1;
            if is_last && msg.role == MessageRole::Assistant {
                msg_h += 2;
            }

            let msg_y = y.max(i32::from(inner_area.y) - 1) as u16;
            let visible_bottom = inner_area.bottom();

            if msg_y < visible_bottom {
                let msg_area = Rect::new(
                    inner_area.x,
                    msg_y,
                    inner_area.width,
                    msg_h.min(i32::from(visible_bottom - msg_y)) as u16,
                );

                match msg.role {
                    MessageRole::User => {
                        let agent_name = msg.agent.as_deref().unwrap_or("default");
                        let agent_color = agent_colors.get(agent_name, &unique_agents);
                        Self::render_user_message(
                            buf,
                            msg_area,
                            msg,
                            theme,
                            agent_color,
                            &self.tool_state,
                            config,
                            false,
                            false,
                        );
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
                            &self.tool_state,
                            config,
                            false,
                            false,
                        );
                    }
                }
            }

            y += msg_h;
        }

        total_height
    }
}
