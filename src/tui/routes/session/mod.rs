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
use cosh_tui::core::types::MouseEvent;

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

/// A rectangular region of text on screen with its content.
struct TextRegion {
    /// Top y-coordinate (inclusive).
    y1: u16,
    /// Bottom y-coordinate (exclusive).
    y2: u16,
    /// Left x-coordinate (inclusive).
    x1: u16,
    /// Right x-coordinate (exclusive).
    x2: u16,
    /// The plain text content displayed in this region.
    text: String,
}

pub struct SessionView {
    pub scroll_y: i32,
    pub tool_state: ToolRenderState,
    /// Text regions from the last render pass, used for mouse-based selection.
    text_regions: Vec<TextRegion>,
    /// Active drag selection: (`anchor_x`, `anchor_y`, `focus_x`, `focus_y`) in screen coordinates.
    /// Stored WITHOUT normalisation so the renderer can apply flow-based highlighting:
    /// top line from `start_x` to end, bottom line from start to `end_x`, middle lines fully highlighted.
    /// Set before `render()` to enable visual selection highlight.
    pub drag_selection: Option<(u16, u16, u16, u16)>,
}

impl SessionView {
    pub fn new() -> Self {
        SessionView {
            scroll_y: 0,
            tool_state: ToolRenderState::new(),
            text_regions: Vec::new(),
            drag_selection: None,
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
                    let h = Self::estimate_part_height(part, max_w, config).min(bottom - y);
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
                    let h = Self::estimate_part_height(part, max_w, config)
                        .min(bottom - y)
                        .max(1);
                    let text_style = Style::default()
                        .fg(rgba_color(fg_color))
                        .bg(rgba_color(theme.background));
                    let rendered = Self::draw_text_wrap(buf, &content, x, y, max_w, h, text_style);
                    y += rendered;
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

    fn draw_text_wrap(
        buf: &mut Buffer,
        text: &str,
        x: u16,
        y_: u16,
        max_w: u16,
        max_h: u16,
        style: Style,
    ) -> u16 {
        let right = x + max_w;
        let bottom = y_ + max_h;
        // Pre-fill area with bg so there's no gap between characters
        for row in y_..bottom {
            for col in x..right {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(style);
                    cell.set_char(' ');
                }
            }
        }
        let mut y = y_;
        let mut cx = x;
        for ch in text.chars() {
            if ch == '\n' {
                y += 1;
                cx = x;
                if y >= bottom {
                    break;
                }
                continue;
            }
            // ratatui panics on control chars, so filter those out
            if ch.is_control() && ch != '\t' {
                continue;
            }
            if cx >= right {
                y += 1;
                cx = x;
                if y >= bottom {
                    break;
                }
                if ch == ' ' {
                    continue;
                }
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            cx += 1;
        }
        (y - y_).max(1)
    }

    fn estimate_part_height(part: &Part, max_w: u16, config: &TuiConfig) -> u16 {
        match part {
            Part::Text(t) if !t.synthetic => {
                // Estimate wrapped height: each line can hold up to max_w chars
                let chars_per_line = max_w as usize;
                if chars_per_line > 0 {
                    let mut total_lines: usize = 0;
                    for line in t.text.lines() {
                        let line_len = line.chars().count();
                        total_lines += if line_len == 0 {
                            1
                        } else {
                            line_len.div_ceil(chars_per_line)
                        };
                    }
                    total_lines.max(1) as u16
                } else {
                    1
                }
            }
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

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cast_sign_loss
    )]
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
        let is_error = msg.id.starts_with("msg-err-");
        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6);

        let banner_h = u16::from(is_compacted);
        let inner_y = if is_error {
            area.y + 2 // vertical padding + border line
        } else {
            area.y + banner_h
        };

        if is_error {
            // Error messages get a red left border
            let mut border_box = BoxRenderable::new();
            border_box.set_background_color(Some(theme.background_panel.into()));
            border_box.set_border_color(Some(theme.error.into()));
            border_box.set_border_sides(BorderSidesConfig {
                left: true,
                top: false,
                right: false,
                bottom: false,
            });
            border_box.set_custom_border_chars(left_border_chars());
            border_box.render_self(buf, area);

            // Render error text character by character with wrap (safe for multi-byte UTF-8)
            let error_style = Style::default()
                .fg(rgba_color(theme.text_muted))
                .bg(rgba_color(theme.background));
            let error_text: String = msg
                .parts
                .iter()
                .filter_map(|p| {
                    if let crate::types::Part::Text(t) = p {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let mut line_x = x_off;
            let mut line_y = area.y + 2;
            for ch in error_text.chars() {
                if ch == '\n' {
                    line_x = x_off;
                    line_y += 1;
                    continue;
                }
                // ratatui panics on control chars, so filter those out
                if ch.is_control() && ch != '\n' && ch != '\t' {
                    continue;
                }
                if line_x >= x_off + max_w {
                    line_x = x_off;
                    line_y += 1;
                }
                if line_y >= area.bottom() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((line_x, line_y)) {
                    cell.set_char(ch);
                    cell.set_style(error_style);
                }
                line_x += 1;
            }
            return;
        }

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

        if is_last && !is_error {
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
    /// Handle a mouse click on the session view.
    /// Returns true if the click was consumed (e.g., toggled a tool expand/collapse).
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        state: &AppState,
        config: &TuiConfig,
    ) -> bool {
        let Some(session) = state.current_session() else {
            return false;
        };

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );
        let max_w = inner_area.width.saturating_sub(6);
        let x_off = inner_area.x + 3;

        let mut y = i32::from(inner_area.y) - self.scroll_y;
        let visible_bottom = inner_area.bottom();
        let click_x = mouse.x;
        let click_y = mouse.y;

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let mut msg_h = 2i32;
            for part in &msg.parts {
                msg_h += i32::from(Self::estimate_part_height(part, max_w, config));
            }
            let is_last = idx == session.messages.len() - 1;
            if is_last && msg.role == crate::types::MessageRole::Assistant {
                msg_h += 2;
            }

            let msg_y = y.max(i32::from(inner_area.y) - 1) as u16;

            if click_y >= msg_y && msg_y < visible_bottom {
                // Check if click is on this message
                // Iterate through parts to find the click target
                let mut part_y = msg_y + 2; // offset for message border

                for part in &msg.parts {
                    let part_h = Self::estimate_part_height(part, max_w, config).max(1);

                    if click_y >= part_y && click_y < part_y + part_h {
                        // Click is within this part
                        if let crate::types::Part::Tool(tool) = part {
                            // Check for shell tool expand/collapse
                            if tool_render::tool_display(&tool.tool) == "bash" {
                                let output =
                                    tool.output.as_deref().unwrap_or("").trim().to_string();
                                if !output.is_empty() {
                                    let id = tool.tool_call_id.as_deref().unwrap_or("shell");
                                    let collapsed =
                                        crate::util::scroll::collapse_tool_output(&output, 10, 800);
                                    if collapsed.overflow {
                                        let expanded = self.tool_state.is_expanded(id);
                                        let display =
                                            if expanded { &output } else { &collapsed.output };
                                        let hint_y = part_y + 1 + display.lines().count() as u16;

                                        if click_y == hint_y
                                            && click_x >= x_off
                                            && click_x < x_off + max_w
                                        {
                                            self.tool_state.toggle_expanded(id);
                                            return true;
                                        }
                                    }
                                }
                            }
                        }

                        // Check reasoning part click
                        if let crate::types::Part::Reasoning(r) = part {
                            let part_id = &r.text[..r.text.len().min(32)];
                            if click_y == part_y {
                                // Click on the reasoning header to toggle
                                let header_x_end = x_off + 8; // "+ Thought" or "- Thought"
                                if click_x >= x_off && click_x < header_x_end {
                                    self.tool_state.toggle_expanded(part_id);
                                    return true;
                                }
                            }
                        }

                        return true; // Click consumed even if not on a clickable region
                    }

                    part_y += part_h;
                }
            }

            y += msg_h;
        }

        false
    }

    /// Build the list of text regions for mouse-based selection.
    #[allow(clippy::too_many_lines, clippy::cast_sign_loss)]
    pub fn build_text_regions(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
    ) {
        self.text_regions.clear();

        let scroll = self.scroll_y;
        let x_off = inner_area.x + 3;

        let mut y = i32::from(inner_area.y) - scroll;

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            // In build_text_regions we don't add +2 for the last assistant message's
            // metadata footer because we don't capture that footer text as regions.
            let mut msg_h = 2i32;
            for part in &msg.parts {
                msg_h += i32::from(Self::estimate_part_height(part, max_w, config));
            }

            let msg_y = y.max(i32::from(inner_area.y) - 1) as u16;
            let visible_bottom = inner_area.bottom();

            if msg_y < visible_bottom {
                // Account for border offsets based on message type.
                let border_offset = if msg.role == MessageRole::User {
                    1
                } else if msg.id.starts_with("msg-err-") {
                    2
                } else {
                    0
                };
                let mut part_y = i32::from(msg_y) + border_offset;

                for part in &msg.parts {
                    let part_h = i32::from(Self::estimate_part_height(part, max_w, config).max(1));
                    let p_y1 = part_y;
                    let p_y2 = part_y + part_h;
                    let vp_y1 = p_y1.max(i32::from(inner_area.y)) as u16;
                    let vp_y2 = p_y2.min(i32::from(inner_area.bottom())) as u16;
                    if vp_y1 >= vp_y2 {
                        part_y += part_h;
                        continue;
                    }

                    match part {
                        crate::types::Part::Text(t) if !t.synthetic => {
                            let content = if config.conceal {
                                conceal_text(&t.text)
                            } else {
                                t.text.clone()
                            };
                            if content.chars().all(char::is_whitespace) {
                                part_y += part_h;
                                continue;
                            }
                            // Split content into visual wrapped lines, exactly like
                            // draw_text_wrap displays them: each screen line is at
                            // most `max_w` chars. Store each visual line as its
                            // own TextRegion with 1-line height so get_text_in_region
                            // can precisely slice characters by x-coordinate.
                            let max_w_usize = max_w as usize;
                            let mut line_y = vp_y1;
                            for logical_line in content.lines() {
                                if logical_line.is_empty() {
                                    if line_y < vp_y2 {
                                        self.text_regions.push(TextRegion {
                                            y1: line_y,
                                            y2: line_y + 1,
                                            x1: x_off,
                                            x2: x_off + max_w,
                                            text: String::new(),
                                        });
                                        line_y += 1;
                                    }
                                    continue;
                                }
                                let mut remaining = logical_line;
                                while !remaining.is_empty() && line_y < vp_y2 {
                                    let n = remaining.chars().take(max_w_usize).count();
                                    let split = remaining
                                        .char_indices()
                                        .nth(n)
                                        .map_or(remaining.len(), |(i, _)| i);
                                    let visual_line = &remaining[..split];
                                    self.text_regions.push(TextRegion {
                                        y1: line_y,
                                        y2: line_y + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text: visual_line.to_string(),
                                    });
                                    line_y += 1;
                                    remaining = &remaining[split..];
                                }
                            }
                        }
                        crate::types::Part::Tool(t) => {
                            if !config.show_generic_tool_output
                                && crate::util::tool_render::tool_display(&t.tool) == "generic"
                            {
                                part_y += part_h;
                                continue;
                            }
                            if let Some(ref output) = t.output {
                                let trimmed = output.trim();
                                if !trimmed.is_empty() {
                                    let display = if config.show_tool_details
                                        || !matches!(t.status, crate::types::ToolStatus::Completed)
                                    {
                                        trimmed.to_string()
                                    } else {
                                        crate::util::scroll::collapse_tool_output(trimmed, 10, 800)
                                            .output
                                    };
                                    // Store each line of tool output separately.
                                    let mut line_y = vp_y1;
                                    for display_line in display.lines() {
                                        if line_y < vp_y2 {
                                            self.text_regions.push(TextRegion {
                                                y1: line_y,
                                                y2: line_y + 1,
                                                x1: x_off,
                                                x2: x_off + max_w,
                                                text: display_line.to_string(),
                                            });
                                            line_y += 1;
                                        }
                                    }
                                }
                            }
                        }
                        crate::types::Part::Reasoning(r) => {
                            let expanded = config.thinking_mode
                                || self.tool_state.is_expanded(&r.text[..r.text.len().min(32)]);
                            let header = if expanded { "- Thought" } else { "+ Thought" };
                            if vp_y1 < vp_y2 {
                                self.text_regions.push(TextRegion {
                                    y1: vp_y1,
                                    y2: vp_y1 + 1,
                                    x1: x_off,
                                    x2: x_off + max_w,
                                    text: header.to_string(),
                                });
                            }
                            if expanded && !r.text.is_empty() {
                                let mut line_y = vp_y1 + 1;
                                let truncated =
                                    r.text.lines().take(10).collect::<Vec<_>>().join("\n");
                                for line in truncated.lines() {
                                    if line_y < vp_y2 && !line.is_empty() {
                                        self.text_regions.push(TextRegion {
                                            y1: line_y,
                                            y2: line_y + 1,
                                            x1: x_off + 2,
                                            x2: x_off + max_w,
                                            text: line.to_string(),
                                        });
                                        line_y += 1;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }

                    part_y += part_h;
                }
            }

            y += msg_h;
        }
    }

    /// Return the text selected by a flow-based selection from `anchor` to `focus`.
    /// Unlike a rectangular selection, flow selection follows the text direction:
    ///   - If selecting top-to-bottom: top line selects from `anchor_x` to end,
    ///     bottom line selects from 0 to `focus_x`, middle lines are fully selected.
    ///   - If selecting bottom-to-top: top line selects from 0 to `focus_x`,
    ///     bottom line selects from `anchor_x` to end, middle lines are fully selected.
    ///     Each `TextRegion` stores exactly 1 visual line, so x-coordinates are
    ///     used to slice individual characters from each line.
    pub fn get_text_in_region(
        &self,
        anchor_x: u16,
        anchor_y: u16,
        focus_x: u16,
        focus_y: u16,
    ) -> String {
        let start_y = anchor_y.min(focus_y);
        let end_y = anchor_y.max(focus_y);

        // start_x is the x-coordinate on the topmost line;
        // end_x is the x-coordinate on the bottommost line.
        let (start_x, end_x) = if anchor_y == start_y {
            // Anchor is at top (or on same line), focus is at bottom
            (anchor_x, focus_x)
        } else {
            // Focus is at top, anchor is at bottom
            (focus_x, anchor_x)
        };

        let mut result = String::new();
        for region in &self.text_regions {
            // Y-range check: does this visual line fall within the selection?
            if region.y1 > end_y || region.y2 <= start_y {
                continue;
            }

            let line_y = region.y1; // Each region is exactly 1 line high

            // Determine the x-range for this specific line based on flow selection.
            let (lx1, lx2) = if start_y == end_y {
                // Single line: just the range between start and end.
                (start_x.min(end_x), start_x.max(end_x))
            } else if line_y == start_y {
                // Topmost selected line: from start_x to end of this line.
                (start_x, region.x2)
            } else if line_y == end_y {
                // Bottommost selected line: from start of this line to end_x.
                (region.x1, end_x)
            } else {
                // Middle line: select the entire line.
                (region.x1, region.x2)
            };

            // Clamp to the actual region's x bounds.
            let ox1 = region.x1.max(lx1);
            let ox2 = region.x2.min(lx2);
            if ox1 >= ox2 {
                continue;
            }

            // Map screen x-coordinates to character indices.
            let col_start = (ox1 - region.x1) as usize;
            let col_end = (ox2 - region.x1) as usize;

            let chars: Vec<char> = region.text.chars().collect();
            let line_len = chars.len();

            if col_start >= line_len {
                continue;
            }
            let end = col_end.min(line_len);

            let sliced: String = chars[col_start..end].iter().collect();
            if !sliced.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&sliced);
            }
        }
        result
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

        // Build text regions for mouse-based selection.
        self.build_text_regions(session, inner_area, max_w, config);

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

        // ── Visual selection highlight (flow-based) ────────────────────────
        // After rendering all messages, apply inverted colors to non-space cells
        // within the flow-based selection region. Uses the same anchor/focus
        // logic as get_text_in_region() so the visual highlight exactly matches
        // what will be copied.
        if let Some((anchor_x, anchor_y, focus_x, focus_y)) = self.drag_selection {
            let start_y = anchor_y.min(focus_y);
            let end_y = anchor_y.max(focus_y);

            let (start_x, end_x) = if anchor_y == start_y {
                (anchor_x, focus_x)
            } else {
                (focus_x, anchor_x)
            };

            // Bounds for "full width" highlight: the session area's right edge.
            let max_x = area.right().saturating_sub(1);
            let min_x = area.x;

            // Apply highlight per-line, matching flow selection boundaries.
            for cy in start_y..=end_y {
                let (lx1, lx2) = if start_y == end_y {
                    // Single line.
                    let a = start_x.min(end_x);
                    let b = start_x.max(end_x);
                    (a.min(max_x), b.min(max_x))
                } else if cy == start_y {
                    // Topmost selected line: from start_x to end.
                    (start_x.min(max_x), max_x)
                } else if cy == end_y {
                    // Bottommost selected line: from start to end_x.
                    (min_x, end_x.min(max_x))
                } else {
                    // Middle line: highlight entire row within session area.
                    (min_x, max_x)
                };

                for cx in lx1..=lx2 {
                    if let Some(cell) = buf.cell_mut((cx, cy))
                        && cell.symbol() != " "
                    {
                        let fg = cell.fg;
                        let bg = cell.bg;
                        cell.set_fg(bg);
                        cell.set_bg(fg);
                    }
                }
            }
        }

        total_height
    }
}
