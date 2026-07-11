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

use cosh_tui::core::renderables::markdown::estimate_height;

use crate::config::TuiConfig;
use crate::state::AppState;
use crate::theme::Theme;
use crate::types::{AgentColors, FilePart, Message, MessageRole, Part, ReasoningPart, ToolStatus};
use crate::util::tool_render::{self, ToolRenderState};

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

const fn concealed_char(ch: char) -> char {
    if ch == ' ' { ' ' } else { '\u{2588}' }
}

fn sanitize_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect()
}

fn conceal_text(text: &str) -> String {
    text.chars().map(concealed_char).collect()
}

struct TextRegion {
    y1: u16,
    y2: u16,
    x1: u16,
    x2: u16,
    text: String,
}

pub struct SessionView {
    pub scroll_y: i32,
    pub tool_state: ToolRenderState,
    text_regions: Vec<TextRegion>,
    pub drag_selection: Option<(u16, u16, u16, u16)>,

    pub is_auto_scrolling: bool,
    auto_scroll_speed: f64,
    auto_scroll_accumulator: f64,
    pub session_area: Option<(u16, u16, u16, u16)>,
    auto_scroll_threshold: u16,
    auto_scroll_speed_slow: f64,
    auto_scroll_speed_medium: f64,
    auto_scroll_speed_fast: f64,

    /// Total content height from last render (used by app.rs for scroll calcs).
    pub total_height: i32,
    /// Visible viewport height from last render.
    pub visible_height: i32,

    // ── Sticky scroll (auto-scroll to bottom) ──────────────────────────────────
    /// Whether the user has manually scrolled away from the sticky position.
    pub has_manual_scroll: bool,
    /// Whether we are currently stuck to the bottom (sticky position).
    pub is_sticky_bottom: bool,
    /// Previous content height to detect size changes.
    last_content_height: i32,
    /// Guard flag that prevents scroll changes from being treated as manual.
    is_applying_sticky_scroll: bool,

    // ── Scroll accumulator (fractional smoothing, like OpenCode) ───────────────
    scroll_accumulator_y: f64,
    /// Scroll speed multiplier (acceleration, default 3.0 = CustomSpeedScroll(3)).
    scroll_accel: f64,
}

impl SessionView {
    pub fn new() -> Self {
        Self {
            scroll_y: 0,
            tool_state: ToolRenderState::new(),
            text_regions: Vec::new(),
            drag_selection: None,
            is_auto_scrolling: false,
            auto_scroll_speed: 0.0,
            auto_scroll_accumulator: 0.0,
            session_area: None,
            auto_scroll_threshold: 3,
            auto_scroll_speed_slow: 6.0,
            auto_scroll_speed_medium: 36.0,
            auto_scroll_speed_fast: 72.0,
            total_height: 0,
            visible_height: 0,
            has_manual_scroll: false,
            is_sticky_bottom: true,
            last_content_height: 0,
            is_applying_sticky_scroll: false,
            scroll_accumulator_y: 0.0,
            scroll_accel: 3.0,
        }
    }

    // ── Scroll control (port of OpenCode's ScrollBox) ──────────────────────────

    /// Accelerated scroll (for mouse wheel). Multiplies delta by `scroll_accel`.
    /// Uses fractional accumulator for smooth scrolling.
    /// Mirrors OpenCode's `onMouseEvent` for scroll type.
    pub fn scroll_by(&mut self, delta: f64) {
        let max_scroll = (self.total_height - self.visible_height).max(0);

        let scroll_amount = delta * self.scroll_accel;
        self.scroll_accumulator_y += scroll_amount;
        let int_scroll = self.scroll_accumulator_y.trunc() as i32;
        if int_scroll != 0 {
            self.scroll_accumulator_y -= int_scroll as f64;
            self.scroll_y = (self.scroll_y + int_scroll).clamp(0, max_scroll);
        }

        self.sync_manual_scroll_state();
    }

    /// Raw scroll (for keyboard / programmatic). No acceleration, uses accumulator.
    /// Mirrors OpenCode's `scrollBy` + `handleKeyPress` pattern.
    pub fn scroll_by_raw(&mut self, delta: f64) {
        let max_scroll = (self.total_height - self.visible_height).max(0);

        self.scroll_accumulator_y += delta;
        let int_scroll = self.scroll_accumulator_y.trunc() as i32;
        if int_scroll != 0 {
            self.scroll_accumulator_y -= int_scroll as f64;
            self.scroll_y = (self.scroll_y + int_scroll).clamp(0, max_scroll);
        }

        self.sync_manual_scroll_state();
    }

    /// Reset the scroll accumulator (called after keyboard scroll, like OpenCode).
    pub fn reset_scroll_accumulator(&mut self) {
        self.scroll_accumulator_y = 0.0;
    }

    /// Set scroll acceleration multiplier (default 3.0 = CustomSpeedScroll(3)).
    pub fn set_scroll_accel(&mut self, accel: f64) {
        self.scroll_accel = accel;
    }

    /// Absolute scroll to position. Mirrors OpenCode's `scrollTo`.
    pub fn scroll_to(&mut self, position: i32) {
        let max_scroll = (self.total_height - self.visible_height).max(0);
        self.scroll_y = position.clamp(0, max_scroll);
        self.scroll_accumulator_y = 0.0;
        self.sync_manual_scroll_state();
    }

    /// Scroll to bottom. Mirrors OpenCode's `toBottom()`:
    ///   `scroll.scrollTo(scroll.scrollHeight)`
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = (self.total_height - self.visible_height).max(0);
        self.scroll_y = max_scroll;
        self.scroll_accumulator_y = 0.0;
        self.has_manual_scroll = false;
        self.is_sticky_bottom = true;
    }

    /// Sync manual scroll state. Mirrors OpenCode's `syncManualScrollState()`.
    /// Sets `hasManualScroll = hasScrollableContent && !isAtStickyPosition()`.
    /// The `is_applying_sticky_scroll` guard prevents this from being called
    /// during `applyStickyStart`/`recalculateBarProps`.
    fn sync_manual_scroll_state(&mut self) {
        if self.is_applying_sticky_scroll {
            return;
        }

        let max_scroll = (self.total_height - self.visible_height).max(0);
        let has_scrollable_content = max_scroll > 1;

        if has_scrollable_content && !self.is_at_sticky_position() {
            self.has_manual_scroll = true;
        } else {
            self.has_manual_scroll = false;
        }

        self.update_sticky_state();
    }

    /// Update sticky state flags. Mirrors OpenCode's `updateStickyState()`.
    fn update_sticky_state(&mut self) {
        let max_scroll = (self.total_height - self.visible_height).max(0);

        if self.scroll_y <= 0 {
            self.is_sticky_bottom = false;
        } else if self.scroll_y >= max_scroll {
            self.is_sticky_bottom = true;
        } else {
            self.is_sticky_bottom = false;
        }
    }

    /// Check if at sticky position. Mirrors OpenCode's `isAtStickyPosition()`.
    /// For "bottom": `scrollTop >= maxScrollTop` (accepts >=, not strict equality).
    fn is_at_sticky_position(&self) -> bool {
        let max_scroll = (self.total_height - self.visible_height).max(0);

        // stickyStart = "bottom"
        if max_scroll <= 0 {
            return true;
        }
        self.scroll_y >= max_scroll
    }

    /// Check if at sticky re-engage point. Mirrors OpenCode's `isAtStickyReengagePoint()`.
    /// For "bottom": `maxScrollTop > 0 && scrollTop >= maxScrollTop - 1`
    pub fn is_at_bottom(&self) -> bool {
        let max_scroll = (self.total_height - self.visible_height).max(0);
        if max_scroll <= 0 {
            return true;
        }
        self.scroll_y >= max_scroll.saturating_sub(1)
    }

    /// Apply sticky start (scroll to bottom). Mirrors OpenCode's `applyStickyStart("bottom")`.
    /// Sets `is_applying_sticky_scroll` guard to prevent recursive sync.
    fn apply_sticky_start(&mut self) {
        let was_applying = self.is_applying_sticky_scroll;
        self.is_applying_sticky_scroll = true;

        let max_scroll = (self.total_height - self.visible_height).max(0);
        self.scroll_y = max_scroll;
        self.is_sticky_bottom = true;

        self.is_applying_sticky_scroll = was_applying;
    }

    /// Recalculate bar props (called when content/viewport size changes).
    /// Mirrors OpenCode's `recalculateBarProps()` which:
    /// 1. Sets `is_applying_sticky_scroll = true`
    /// 2. If `!hasManualScroll` → `applyStickyStart(stickyStart)`
    /// 3. If `hasManualScroll && isAtStickyReengagePoint()` → re-engage
    /// 4. Updates `last_content_height`
    fn recalculate_bar_props(&mut self, total_height: i32, visible_height: i32) {
        let was_applying = self.is_applying_sticky_scroll;
        self.is_applying_sticky_scroll = true;

        let new_max_scroll = (total_height - visible_height).max(0);

        if !self.has_manual_scroll {
            // No manual scroll → apply sticky start
            self.apply_sticky_start();
        } else if self.is_at_bottom() && new_max_scroll > 0 {
            // User scrolled back to bottom during streaming → re-engage sticky
            self.has_manual_scroll = false;
            self.scroll_y = new_max_scroll;
            self.is_sticky_bottom = true;
        }

        self.is_applying_sticky_scroll = was_applying;
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
            md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(255, 200, 0, 255))));
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
                        sanitize_text(&t.text)
                    };
                    let h = Self::estimate_part_height(part, max_w, config, role).min(bottom - y);
                    let area = Rect::new(x, y, max_w, h);
                    let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                        Some(content),
                    );
                    md.set_fg(Some(ColorInput::RGBA(fg_color)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(255, 200, 0, 255))));
                    md.render_self(buf, area);
                    y += h;
                }
                Part::Text(t) if !t.synthetic => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        sanitize_text(&t.text)
                    };
                    let h = Self::estimate_part_height(part, max_w, config, role)
                        .min(bottom - y)
                        .max(1);
                    let text_style = Style::default()
                        .fg(rgba_color(fg_color))
                        .bg(rgba_color(theme.background_panel));
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

    #[allow(clippy::too_many_arguments)]
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
            if ch.is_control() {
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
        (y - y_ + 1).max(1)
    }

    fn estimate_part_height(
        part: &Part,
        max_w: u16,
        config: &TuiConfig,
        role: &MessageRole,
    ) -> u16 {
        match part {
            Part::Text(t) if !t.synthetic && *role == MessageRole::Assistant => {
                estimate_height(&t.text, max_w)
            }
            Part::Text(t) if !t.synthetic => {
                let chars_per_line = max_w as usize;
                if chars_per_line > 0 {
                    let mut total_lines: usize = 0;
                    for line in t.text.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        let line_len = line.chars().count();
                        total_lines += line_len.div_ceil(chars_per_line);
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
                    let output = t.output.as_deref().unwrap_or("").trim();
                    let collapsed = crate::util::scroll::collapse_tool_output(output, 10, 800);
                    let display = &collapsed.output;
                    let lines = display.lines().count().max(1) as u16;
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
        let border_line = 1u16;
        let padding_bottom = 1u16;
        let inner_h = area
            .height
            .saturating_sub(border_line)
            .saturating_sub(padding_bottom);

        if is_compacted {
            Self::render_compaction_banner(buf, x_off, area.y + 1, max_w, theme);
        }

        let inner_y = area.y + border_line + u16::from(is_compacted);
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
        tool_state: &ToolRenderState,
        config: &TuiConfig,
        is_queued: bool,
        is_compacted: bool,
    ) {
        let is_error = msg.id.starts_with("msg-err-");
        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6);

        if is_error {
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

            let error_style = Style::default()
                .fg(rgba_color(theme.text_muted))
                .bg(rgba_color(theme.background_panel));
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
                .join(" ")
                .chars()
                .filter(|ch| !ch.is_control())
                .collect();
            let mut line_x = x_off;
            let mut line_y = area.y + 1;
            for ch in error_text.chars() {
                if ch == '\n' {
                    line_x = x_off;
                    line_y += 1;
                    continue;
                }
                if ch.is_control() {
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

        let banner_h = u16::from(is_compacted);
        let inner_y = area.y + banner_h;
        let net_h = area.height.saturating_sub(banner_h);

        Self::render_parts(
            buf,
            x_off,
            inner_y,
            max_w,
            net_h,
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
    }

    #[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
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
        let x_off = i32::from(inner_area.x + 3);
        let max_w_i32 = i32::from(max_w);

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());
        let mut y = vp_top - self.scroll_y;
        let click_x = i32::from(mouse.x);
        let click_y = i32::from(mouse.y);

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let msg_h = Self::render_message_height(msg, max_w, config);
            let msg_top = y;
            let msg_bottom = y + msg_h;

            // Check if click is within this message and message is visible
            if click_y >= msg_top
                && click_y < msg_bottom
                && msg_bottom > vp_top
                && msg_top < vp_bottom
            {
                let border_offset: i32 = match msg.role {
                    MessageRole::User => 1,
                    MessageRole::Assistant => 0,
                };
                let mut part_y = msg_top + border_offset;

                for part in &msg.parts {
                    let part_h = i32::from(
                        Self::estimate_part_height(part, max_w, config, &msg.role).max(1),
                    );

                    if click_y >= part_y && click_y < part_y + part_h {
                        if let crate::types::Part::Tool(tool) = part
                            && tool_render::tool_display(&tool.tool) == "bash"
                        {
                            let output = tool.output.as_deref().unwrap_or("").trim().to_string();
                            if !output.is_empty() {
                                let id = tool.tool_call_id.as_deref().unwrap_or("shell");
                                let collapsed =
                                    crate::util::scroll::collapse_tool_output(&output, 10, 800);
                                if collapsed.overflow {
                                    let expanded = self.tool_state.is_expanded(id);
                                    let display =
                                        if expanded { &output } else { &collapsed.output };
                                    let hint_y = part_y + 1 + display.lines().count() as i32;

                                    if click_y == hint_y
                                        && click_x >= x_off
                                        && click_x < x_off + max_w_i32
                                    {
                                        self.tool_state.toggle_expanded(id);
                                        return true;
                                    }
                                }
                            }
                        }

                        if let crate::types::Part::Reasoning(r) = part {
                            let part_id = &r.text[..r.text.len().min(32)];
                            if click_y == part_y {
                                let header_x_end = x_off + 8;
                                if click_x >= x_off && click_x < header_x_end {
                                    self.tool_state.toggle_expanded(part_id);
                                    return true;
                                }
                            }
                        }

                        return true;
                    }

                    part_y += part_h;
                }
            }

            y += msg_h;
        }

        false
    }

    pub fn render_message_height(msg: &Message, max_w: u16, config: &TuiConfig) -> i32 {
        let border_h: i32 = match msg.role {
            MessageRole::User => 1,
            MessageRole::Assistant => 0,
        };
        let is_error = msg.id.starts_with("msg-err-");
        let parts_h: i32 = if is_error {
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
                .join(" ")
                .chars()
                .filter(|ch| !ch.is_control())
                .collect();
            let mut lines = 0u16;
            let mut col = 0u16;
            for ch in error_text.chars() {
                if ch == '\n' {
                    lines += 1;
                    col = 0;
                    continue;
                }
                if ch.is_control() {
                    continue;
                }
                if col >= max_w {
                    lines += 1;
                    col = 0;
                }
                col += 1;
            }
            if col > 0 || error_text.is_empty() {
                lines += 1;
            }
            i32::from(lines)
        } else {
            msg.parts
                .iter()
                .map(|p| i32::from(Self::estimate_part_height(p, max_w, config, &msg.role)))
                .sum()
        };
        let padding_bottom: i32 = match msg.role {
            MessageRole::User => 1,
            MessageRole::Assistant if is_error => 2,
            MessageRole::Assistant => 0,
        };
        border_h + parts_h + padding_bottom
    }

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
        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());

        let mut y = vp_top - scroll;

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let msg_h = Self::render_message_height(msg, max_w, config);
            let msg_top = y;
            let msg_bottom = y + msg_h;

            // Only process messages that overlap with the viewport
            if msg_bottom > vp_top && msg_top < vp_bottom {
                let border_offset = match msg.role {
                    MessageRole::User => 1,
                    MessageRole::Assistant => 0,
                };
                // Adjust part_y so that parts before the viewport are accounted for
                let mut part_y = msg_top + border_offset;

                for part in &msg.parts {
                    let part_h = i32::from(
                        Self::estimate_part_height(part, max_w, config, &msg.role).max(1),
                    );
                    let p_top = part_y;
                    let p_bottom = part_y + part_h;

                    // Clip part to viewport
                    let vp_y1 = p_top.max(vp_top) as u16;
                    let vp_y2 = p_bottom.min(vp_bottom) as u16;

                    if vp_y1 < vp_y2 {
                        match part {
                            crate::types::Part::Text(t) if !t.synthetic => {
                                let content = if config.conceal {
                                    conceal_text(&t.text)
                                } else {
                                    sanitize_text(&t.text)
                                };
                                if content.chars().all(char::is_whitespace) {
                                    part_y += part_h;
                                    continue;
                                }
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
                                            || !matches!(
                                                t.status,
                                                crate::types::ToolStatus::Completed
                                            ) {
                                            trimmed.to_string()
                                        } else {
                                            crate::util::scroll::collapse_tool_output(
                                                trimmed, 10, 800,
                                            )
                                            .output
                                        };
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
                    }

                    part_y += part_h;
                }
            }

            y += msg_h;
        }
    }

    pub fn get_text_in_region(
        &self,
        anchor_x: u16,
        anchor_y: u16,
        focus_x: u16,
        focus_y: u16,
    ) -> String {
        let start_y = anchor_y.min(focus_y);
        let end_y = anchor_y.max(focus_y);

        let (start_x, end_x) = if anchor_y == start_y {
            (anchor_x, focus_x)
        } else {
            (focus_x, anchor_x)
        };

        let mut result = String::new();
        for region in &self.text_regions {
            if region.y1 > end_y || region.y2 <= start_y {
                continue;
            }

            let line_y = region.y1;

            let (lx1, lx2) = if start_y == end_y {
                (start_x.min(end_x), start_x.max(end_x))
            } else if line_y == start_y {
                (start_x, region.x2)
            } else if line_y == end_y {
                (region.x1, end_x)
            } else {
                (region.x1, region.x2)
            };

            let ox1 = region.x1.max(lx1);
            let ox2 = region.x2.min(lx2);
            if ox1 >= ox2 {
                continue;
            }

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

    #[allow(
        clippy::cast_sign_loss,
        clippy::too_many_lines,
        clippy::too_many_arguments
    )]
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        config: &TuiConfig,
        delta_time: f64,
    ) -> i32 {
        let Some(session) = state.current_session() else {
            return 0;
        };

        self.session_area = Some((area.x, area.y + 1, area.right(), area.bottom()));

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
            let msg_h = Self::render_message_height(msg, max_w, config);
            total_height += gap + msg_h;
        }

        let visible_height = i32::from(inner_area.height);
        let max_scroll = (total_height - visible_height).max(0);
        self.scroll_y = self.scroll_y.clamp(0, max_scroll);

        // Cache dimensions for app.rs
        self.total_height = total_height;
        self.visible_height = visible_height;

        // ── Sticky scroll: recalculateBarProps on content size change ─────────
        // Mirrors OpenCode's `recalculateBarProps()` which calls `applyStickyStart`
        // when content size changes and user hasn't manually scrolled.
        if total_height != self.last_content_height {
            self.recalculate_bar_props(total_height, visible_height);
            self.last_content_height = total_height;
        }

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
        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let msg_h = Self::render_message_height(msg, max_w, config);
            let msg_top = y;
            let msg_bottom = y + msg_h;

            // Check if message overlaps with viewport (using i32, no u16 wrap)
            if msg_bottom > vp_top && msg_top < vp_bottom {
                let is_top_clipped = msg_top < vp_top;

                if is_top_clipped {
                    // ── Top-clipped: render full message into temp buffer, then copy ──
                    // This avoids clipping the area.height (which causes inner_h/padding
                    // miscalculation) and mirrors OpenCode's approach where each child
                    // renders at its natural height and the viewport clips naturally.
                    let src_y = (vp_top - msg_top) as u16; // first visible line in temp
                    let dst_y = vp_top as u16;
                    let vis_h = (msg_bottom.min(vp_bottom) - vp_top) as u16;

                    if vis_h > 0 && msg_h > 0 {
                        let full_area = Rect::new(0, 0, inner_area.width, msg_h as u16);
                        let mut temp = Buffer::empty(full_area);

                        // Fill entire temp buffer with session background to prevent
                        // "holes" when copying. Cells not touched by render functions
                        // (e.g., left margin of assistant messages) maintain this
                        // background instead of default (transparent) style.
                        let session_bg = Style::default().bg(rgba_color(theme.background));
                        for by in 0..full_area.height {
                            for bx in 0..full_area.width {
                                if let Some(cell) = temp.cell_mut((bx, by)) {
                                    cell.set_style(session_bg);
                                    cell.set_char(' ');
                                }
                            }
                        }

                        match msg.role {
                            MessageRole::User => {
                                let agent_name = msg.agent.as_deref().unwrap_or("default");
                                let agent_color = agent_colors.get(agent_name, &unique_agents);
                                Self::render_user_message(
                                    &mut temp,
                                    full_area,
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
                                    &mut temp,
                                    full_area,
                                    msg,
                                    theme,
                                    &self.tool_state,
                                    config,
                                    false,
                                    false,
                                );
                            }
                        }

                        // Copy visible portion from temp buffer to main buffer
                        for dy in 0..vis_h {
                            let temp_y = src_y + dy;
                            let dst_line_y = dst_y + dy;
                            for dx in 0..inner_area.width {
                                if let Some(cell) = temp.cell((dx, temp_y)) {
                                    if let Some(dst) = buf.cell_mut((inner_area.x + dx, dst_line_y))
                                    {
                                        *dst = cell.clone();
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // ── Not top-clipped: render directly to main buffer ──
                    // The message's top is at or below vp_top, so we use its
                    // natural Y position. Bottom clipping is handled naturally
                    // by render_parts (breaks when y >= bottom).
                    let visible_top = msg_top as u16;
                    let visible_bottom = msg_bottom.min(vp_bottom);
                    let visible_h = (visible_bottom - i32::from(visible_top)) as u16;

                    if visible_h > 0 {
                        let msg_area =
                            Rect::new(inner_area.x, visible_top, inner_area.width, visible_h);

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
                                    &self.tool_state,
                                    config,
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                }
            }

            y += msg_h;
        }

        if let Some((anchor_x, anchor_y, focus_x, focus_y)) = self.drag_selection {
            let start_y = anchor_y.min(focus_y);
            let end_y = anchor_y.max(focus_y);

            let (start_x, end_x) = if anchor_y == start_y {
                (anchor_x, focus_x)
            } else {
                (focus_x, anchor_x)
            };

            let max_x = area.right().saturating_sub(1);
            let min_x = area.x;

            for cy in start_y..=end_y {
                let (lx1, lx2) = if start_y == end_y {
                    let a = start_x.min(end_x);
                    let b = start_x.max(end_x);
                    (a.min(max_x), b.min(max_x))
                } else if cy == start_y {
                    (start_x.min(max_x), max_x)
                } else if cy == end_y {
                    (min_x, end_x.min(max_x))
                } else {
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

        self.handle_auto_scroll(delta_time, total_height, visible_height);

        total_height
    }

    const fn get_auto_scroll_direction(&self, mouse_y: u16) -> i32 {
        let Some((_sx, sy, _sx2, sy2)) = self.session_area else {
            return 0;
        };
        let relative_y = mouse_y.saturating_sub(sy);
        let height = sy2.saturating_sub(sy);
        let threshold = self.auto_scroll_threshold;

        if relative_y <= threshold && self.scroll_y > 0 {
            return -1;
        }
        if height > threshold
            && relative_y >= height.saturating_sub(threshold)
            && self.scroll_y < i32::MAX
        {
            return 1;
        }

        0
    }

    fn get_auto_scroll_speed(&self, mouse_y: u16) -> f64 {
        let Some((_sx, sy, _sx2, sy2)) = self.session_area else {
            return 0.0;
        };
        let relative_y = mouse_y.saturating_sub(sy);
        let height = sy2.saturating_sub(sy);

        let dist_to_top = relative_y;
        let dist_to_bottom = height.saturating_sub(relative_y);
        let min_distance = dist_to_top.min(dist_to_bottom);

        if min_distance <= 1 {
            self.auto_scroll_speed_fast
        } else if min_distance <= 2 {
            self.auto_scroll_speed_medium
        } else {
            self.auto_scroll_speed_slow
        }
    }

    pub fn update_auto_scroll(&mut self, mouse_x: u16, mouse_y: u16) {
        let Some((sx, _sy, sx2, _sy2)) = self.session_area else {
            self.stop_auto_scroll();
            return;
        };
        if mouse_x < sx || mouse_x >= sx2 {
            self.stop_auto_scroll();
            return;
        }

        self.auto_scroll_speed = self.get_auto_scroll_speed(mouse_y);
        let dir = self.get_auto_scroll_direction(mouse_y);
        if dir == 0 {
            self.stop_auto_scroll();
        } else if !self.is_auto_scrolling {
            self.is_auto_scrolling = true;
            self.auto_scroll_accumulator = 0.0;
        }
    }

    fn handle_auto_scroll(&mut self, delta_time: f64, total_height: i32, visible_height: i32) {
        if !self.is_auto_scrolling {
            return;
        }

        // Use accumulator pattern (like OpenCode's `handleAutoScroll`)
        let dir = if let Some((_ax, _ay, _fx, fy)) = self.drag_selection {
            self.get_auto_scroll_direction(fy)
        } else {
            0
        };

        if dir == 0 {
            self.stop_auto_scroll();
            return;
        }

        let scroll_amount = self.auto_scroll_speed * delta_time;
        self.auto_scroll_accumulator += scroll_amount * f64::from(dir);

        let int_scroll = self.auto_scroll_accumulator.trunc() as i32;
        if int_scroll != 0 {
            self.auto_scroll_accumulator -= int_scroll as f64;
            let max_scroll = (total_height - visible_height).max(0);
            let new_scroll = (self.scroll_y + int_scroll).clamp(0, max_scroll);
            if new_scroll == self.scroll_y {
                // Already at boundary, stop
                self.stop_auto_scroll();
                return;
            }
            self.scroll_y = new_scroll;
        }
    }

    pub fn stop_auto_scroll(&mut self) {
        if self.is_auto_scrolling {
            self.is_auto_scrolling = false;
            self.auto_scroll_accumulator = 0.0;
            self.auto_scroll_speed = 0.0;
        }
    }
}
