pub mod footer;
pub mod permission;
pub mod question;
pub mod right_panel;
pub mod sidebar;
pub mod subagent_footer;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::scroll_bar::{ScrollBarOrientation, ScrollBarRenderable};
use cosh_tui::core::types::MouseEvent;

use cosh_tui::core::renderables::markdown::estimate_height;

use std::hash::Hasher;

use crate::config::TuiConfig;
use crate::state::AppState;
use crate::theme::Theme;
use crate::types::{
    AgentColors, FilePart, Message, MessageRole, Part, ReasoningPart, SessionStatus, ToolStatus,
};
use crate::util::tool_render::{self, ToolRenderState};
use std::time::Instant;

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
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
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

/// Hash message part content for cache invalidation.
///
/// Hashes the actual content bytes of each part (text body, tool name,
/// reasoning text, filename) using `DefaultHasher::write()`, not just
/// their lengths. This ensures that switching sessions with different
/// content produces different tokens, preventing stale cache hits.
///
/// Without content-aware hashing, two messages with same-length parts
/// would produce identical tokens, causing the per-message render
/// cache to serve stale cells from a different session.
fn hash_parts(msg: &Message) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Hash part count
    hasher.write_usize(msg.parts.len());
    for part in &msg.parts {
        match part {
            Part::Text(t) => {
                // Hash actual content bytes, not just length
                hasher.write(t.text.as_bytes());
                hasher.write(&[t.synthetic as u8]);
            }
            Part::Tool(t) => {
                hasher.write(t.tool.as_bytes());
                hasher.write(&[status_to_u8(&t.status)]);
                if let Some(ref out) = t.output {
                    hasher.write(out.as_bytes());
                }
                if let Some(ref id) = t.tool_call_id {
                    hasher.write(id.as_bytes());
                }
            }
            Part::Reasoning(r) => {
                hasher.write(r.text.as_bytes());
            }
            Part::File(f) => {
                hasher.write(f.filename.as_bytes());
                hasher.write(f.mime.as_bytes());
            }
        }
    }
    hasher.finish()
}

/// Convert a `ToolStatus` to a single byte for hashing.
fn status_to_u8(status: &ToolStatus) -> u8 {
    match status {
        ToolStatus::Running => 0,
        ToolStatus::Completed => 1,
        ToolStatus::Failed(_) => 2,
    }
}

/// Combined hash of part metadata (lengths, statuses, counts).
/// Used to detect streaming/tool-status changes without full re-parse.
fn msg_change_token(msg: &Message) -> u64 {
    hash_parts(msg)
}

fn msg_content_token(msg: &Message, config_token: u64, max_w: u16) -> u64 {
    let mut h = hash_parts(msg);
    h = h.wrapping_mul(31).wrapping_add(config_token);
    h = h.wrapping_mul(31).wrapping_add(max_w as u64);
    h
}

/// A run of rendered text at a given content position.
/// Coordinates are in **content space** (absolute row from start of session content),
/// so they remain valid regardless of the current scroll position.
#[derive(Clone)]
struct TextRegion {
    /// First content row (inclusive).
    y1: i32,
    /// Last content row (exclusive).
    y2: i32,
    x1: u16,
    x2: u16,
    text: String,
}

/// Cached heights per message (avoids duplicate pulldown_cmark parses).
/// Messages are immutable after receipt, so results are valid until
/// config/max_w changes or new messages arrive.
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

    // ── Scroll position at mouse-down (for content-space selection) ────────────
    /// The `scroll_y` value when the current drag selection started.
    /// Used together with `drag_selection` to convert screen-space anchor
    /// coordinates into content-space when extracting selected text.
    pub mouse_down_scroll_y: i32,
    /// Anchor Y in content space (set once at mouse-down), so the visual
    /// highlight scrolls with the content during auto-scroll drag.
    pub selection_anchor_content_y: i32,
    /// Focus Y in content space (updated on each drag), so the visual
    /// highlight follows the content during auto-scroll drag.
    pub selection_focus_content_y: i32,

    // ── Sticky scroll (auto-scroll to bottom) ──────────────────────────────────
    /// Whether the user has manually scrolled away from the sticky position.
    pub has_manual_scroll: bool,
    /// Whether we are currently stuck to the bottom (sticky position).
    pub is_sticky_bottom: bool,
    /// Previous content height to detect size changes.
    last_content_height: i32,
    /// Cached total content height, recomputed only on height cache changes.
    cached_total_height: i32,
    /// Guard flag that prevents scroll changes from being treated as manual.
    is_applying_sticky_scroll: bool,

    // ── Scroll accumulator (fractional smoothing, like OpenCode) ───────────────
    scroll_accumulator_y: f64,
    /// Scroll speed multiplier (acceleration, default 3.0 = CustomSpeedScroll(3)).
    scroll_accel: f64,

    // ── Height cache (avoids duplicate pulldown_cmark parses) ──────────────────
    msg_height_cache: Vec<i32>,
    part_heights_cache: Vec<Vec<u16>>,
    cache_max_w: u16,
    cache_config_token: u64,
    /// Change-detection token of the last message when caches were last built.
    /// Used to detect streaming/tool-status changes without a full cache rebuild.
    last_msg_change_token: u64,
    /// ID of the session for which caches were last built.
    /// Forces a full rebuild when switching sessions with the same message count.
    last_session_id: Option<String>,

    // ── text_regions dirty flag (skip rebuild when nothing changed) ────────────
    text_regions_gen: u64,

    // ── Per-message render cache (skip full re-render of unchanged messages) ───
    msg_cache_tokens: Vec<u64>,
    /// Cached rendered cells per message, flattened row-major (w × h).
    msg_cache_cells: Vec<Option<Vec<ratatui::buffer::Cell>>>,
    msg_cache_w: Vec<u16>,
    msg_cache_h: Vec<u16>,
    /// Cached text regions per message. Populated during the render loop;
    /// consumed by `build_text_regions` to avoid redundant markdown parses.
    msg_cache_text_regions: Vec<Option<Vec<TextRegion>>>,
}

fn text_regions_generation(session: &crate::types::Session, config: &TuiConfig, max_w: u16) -> u64 {
    let mut g: u64 = session.messages.len() as u64;
    g = g.wrapping_mul(31).wrapping_add(max_w as u64);
    g = g.wrapping_mul(31).wrapping_add(config_token(config));
    if let Some(last) = session.messages.last() {
        g = g.wrapping_mul(31).wrapping_add(msg_change_token(last));
    }
    g
}

fn config_token(config: &TuiConfig) -> u64 {
    let mut token: u64 = 0;
    if config.conceal {
        token |= 1;
    }
    if config.show_tool_details {
        token |= 2;
    }
    if config.show_generic_tool_output {
        token |= 4;
    }
    if config.thinking_mode {
        token |= 8;
    }
    token = token.wrapping_mul(31).wrapping_add(config.theme_gen);
    token
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
            mouse_down_scroll_y: 0,
            selection_anchor_content_y: 0,
            selection_focus_content_y: 0,
            has_manual_scroll: false,
            is_sticky_bottom: true,
            last_content_height: 0,
            cached_total_height: 0,
            is_applying_sticky_scroll: false,
            scroll_accumulator_y: 0.0,
            scroll_accel: 3.0,
            msg_height_cache: Vec::new(),
            part_heights_cache: Vec::new(),
            cache_max_w: 0,
            cache_config_token: 0,
            last_msg_change_token: 0,
            text_regions_gen: 0,
            msg_cache_tokens: Vec::new(),
            msg_cache_cells: Vec::new(),
            msg_cache_w: Vec::new(),
            msg_cache_h: Vec::new(),
            msg_cache_text_regions: Vec::new(),
            last_session_id: None,
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

        self.has_manual_scroll = has_scrollable_content && !self.is_at_sticky_position();

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
            self.is_sticky_bottom = self.scroll_y >= max_scroll;
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

    /// Extract text regions from a slice of rendered cells (flattened row-major).
    /// Each output region occupies one content row (y2 = y1 + 1).
    fn cells_to_text_regions(
        cells: &[ratatui::buffer::Cell],
        w: usize,
        h: u16,
        content_start_y: i32,
        x_off: u16,
        text_max_w: u16,
    ) -> Vec<TextRegion> {
        let mut regions = Vec::with_capacity(h as usize);
        for dy in 0..h as usize {
            let mut line_text = String::with_capacity(w);
            let base = dy * w;
            for dx in 0..w {
                line_text.push(cells[base + dx].symbol().chars().next().unwrap_or(' '));
            }
            let trimmed = line_text.trim_end().to_string();
            let cy = content_start_y + dy as i32;
            regions.push(TextRegion {
                y1: cy,
                y2: cy + 1,
                x1: x_off,
                x2: x_off + text_max_w,
                text: trimmed,
            });
        }
        regions
    }

    /// Scan a buffer rectangle [x, x+w) × [y, y+h) and return the
    /// number of rows from y that contain at least one non-space glyph.
    /// Returns 0 if the rectangle is empty or all-space.
    fn scan_content_height(buf: &Buffer, x: u16, y: u16, w: u16, h: u16) -> u16 {
        let mut last_row: Option<u16> = None;
        for check_y in y..y + h {
            for cx in x..x + w {
                if let Some(cell) = buf.cell((cx, check_y))
                    && cell.symbol().chars().next().unwrap_or(' ') != ' '
                {
                    last_row = Some(check_y);
                    break;
                }
            }
        }
        last_row.map(|r| r - y + 1).unwrap_or(0)
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
        opt_heights: Option<&[u16]>,
        streaming: bool,
    ) -> u16 {
        let mut y = y_start;
        let bottom = y_start + max_h;
        let fg_color = if config.conceal {
            theme.text_muted
        } else {
            theme.text
        };

        for (pi, part) in parts.iter().enumerate() {
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
                    let est_h = opt_heights
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| Self::estimate_part_height(part, max_w, config, role));
                    let render_h = (est_h.saturating_add(5))
                        .max(10)
                        .min(bottom.saturating_sub(y));
                    let area = Rect::new(x, y, max_w, render_h);
                    let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                        Some(content),
                    );
                    md.set_fg(Some(ColorInput::RGBA(fg_color)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(
                        255, 200, 0, 255,
                    ))));
                    md.set_streaming(streaming);
                    md.render_self(buf, area);
                    let actual_h = Self::scan_content_height(buf, x, y, max_w, render_h);
                    y += actual_h.max(1);
                }
                Part::Text(t) if !t.synthetic => {
                    let content = if config.conceal {
                        conceal_text(&t.text)
                    } else {
                        sanitize_text(&t.text)
                    };
                    let h = opt_heights
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| Self::estimate_part_height(part, max_w, config, role))
                        .min(bottom - y)
                        .max(1);
                    let text_style = Style::default()
                        .fg(rgba_color(fg_color))
                        .bg(rgba_color(theme.background_panel));
                    let rendered = Self::draw_text_wrap(buf, &content, x, y, max_w, h, text_style);
                    y += rendered;
                }
                Part::Tool(tool) => {
                    // Skip hidden TODO tools entirely (no visual space, no copyable text)
                    if tool_render::tool_display(&tool.tool) == "todo"
                        && !matches!(tool.status, ToolStatus::Failed(_))
                        && (matches!(tool.status, ToolStatus::Running)
                            || tool.output.as_deref().unwrap_or("").trim().is_empty())
                    {
                        continue;
                    }
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

                    // Determine if this tool renders as a block (bordered box)
                    // so we can add a small vertical margin around it.
                    let tool_display = tool_render::tool_display(&tool.tool);
                    let is_block = matches!(tool.status, ToolStatus::Completed)
                        && tool.output.as_deref().is_some_and(|o| !o.trim().is_empty())
                        && matches!(tool_display, "bash" | "write" | "edit" | "todo");

                    // Top margin — skip if this is the first part in the message
                    // or if there isn't room for at least 1 row after it.
                    if is_block && y > y_start && y + 1 < bottom {
                        y += 1;
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
                    let available = bottom.saturating_sub(y);
                    line_h = line_h.min(available);
                    y += line_h;

                    // Bottom margin
                    if is_block && y < bottom {
                        y += 1;
                    }
                }
                Part::Reasoning(r) => {
                    let expanded = config.thinking_mode
                        || tool_state.is_expanded(&r.text[..r.text.floor_char_boundary(32)]);
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
        let Some(right) = x.checked_add(max_w) else {
            return 1;
        };
        let Some(bottom) = y_.checked_add(max_h) else {
            return 1;
        };
        for row in y_..bottom {
            for col in x..right {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(style);
                    cell.set_char(' ');
                }
            }
        }
        let lines = cosh_tui::core::lib::unicode_util::word_wrap(text, max_w);
        let mut y = y_;
        for line in &lines {
            if y >= bottom {
                break;
            }
            let mut cx = x;
            for (grapheme, w) in cosh_tui::core::lib::unicode_util::graphemes_with_width(line) {
                if cx >= right {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    if grapheme.len() == 1 {
                        cell.set_char(grapheme.chars().next().unwrap());
                    } else {
                        cell.set_symbol(grapheme);
                    }
                    cell.set_style(style);
                }
                if w > 1 {
                    for dx in 1..w {
                        if let Some(cell) = buf.cell_mut((cx + dx, y)) {
                            cell.set_diff_option(CellDiffOption::Skip);
                        }
                    }
                }
                cx += w;
            }
            y += 1;
        }
        (y - y_).max(1)
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
                let lines = cosh_tui::core::lib::unicode_util::word_wrap(&t.text, max_w);
                lines.len().max(1) as u16
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
                // Hide TODO tools that are running or have no output (cleared by dedup)
                // — they take no visual space and no copyable text. Only show on failure.
                if tool_render::tool_display(&t.tool) == "todo"
                    && !matches!(t.status, ToolStatus::Failed(_))
                    && (matches!(t.status, ToolStatus::Running)
                        || t.output.as_deref().unwrap_or("").trim().is_empty())
                {
                    return 0;
                }
                // Only tools that render block-style output (shell, write, edit, todo)
                // should allocate height for the full output block. All other tool
                // types render inline (1 line) regardless of whether they have output.
                let is_block = t.output.is_some()
                    && matches!(t.status, ToolStatus::Completed)
                    && !t.output.as_deref().unwrap_or("").trim().is_empty()
                    && matches!(
                        tool_render::tool_display(&t.tool),
                        "bash" | "write" | "edit" | "todo"
                    );
                if is_block {
                    let output = t.output.as_deref().unwrap_or("").trim();
                    // Add 2 rows for the block's internal padding (top/bottom border lines),
                    // plus 2 rows for the external vertical margin that render_parts adds
                    // around block-type tools (1 top, 1 bottom).
                    if tool_render::tool_display(&t.tool) == "todo" {
                        let formatted = tool_render::format_todo_output(output, &t.tool);
                        let lines = formatted.len().max(1) as u16;
                        lines + 4
                    } else if tool_render::tool_display(&t.tool) == "edit" {
                        // Extract diff from JSON (same logic as render_edit) to get
                        // an accurate line count for height estimation.
                        let diff_content = if tool_render::looks_like_unified_diff(output) {
                            Some(output.to_string())
                        } else {
                            tool_render::extract_diff_from_json(output)
                        };
                        let diff_lines = diff_content
                            .as_deref()
                            .map(|d| d.lines().count().min(30) as u16)
                            .unwrap_or(0);
                        // line_h = diff_lines + 3 (padding + title + gap)
                        // + 2 for external margins (top + bottom)
                        diff_lines + 5
                    } else {
                        let collapsed = crate::util::scroll::collapse_tool_output(output, 10, 800);
                        let lines = collapsed.output.lines().count().max(1) as u16
                            + u16::from(collapsed.overflow);
                        lines + 4
                    }
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
        part_heights: Option<&[u16]>,
        streaming: bool,
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
            part_heights,
            streaming,
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
        part_heights: &[u16],
        streaming: bool,
    ) -> u16 {
        let is_error = msg.id.starts_with("msg-err-");
        let x_off = area.x + 3;
        let max_w = area.width.saturating_sub(6).max(2);

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
            let lines = cosh_tui::core::lib::unicode_util::word_wrap(&error_text, max_w);
            let mut line_y = area.y + 1;
            for line in &lines {
                if line_y >= area.bottom() {
                    break;
                }
                let mut line_x = x_off;
                for (grapheme, w) in cosh_tui::core::lib::unicode_util::graphemes_with_width(line) {
                    if line_x > x_off + max_w {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((line_x, line_y)) {
                        if grapheme.len() == 1 {
                            cell.set_char(grapheme.chars().next().unwrap());
                        } else {
                            cell.set_symbol(grapheme);
                        }
                        cell.set_style(error_style);
                    }
                    if w > 1 {
                        for dx in 1..w {
                            if let Some(cell) = buf.cell_mut((line_x + dx, line_y)) {
                                cell.set_diff_option(CellDiffOption::Skip);
                            }
                        }
                    }
                    line_x += w;
                }
                line_y += 1;
            }
            return 0;
        }

        let banner_h = u16::from(is_compacted);
        let inner_y = area.y + banner_h;
        let net_h = area.height.saturating_sub(banner_h);

        let parts_h = Self::render_parts(
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
            Some(part_heights),
            streaming,
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

        banner_h + parts_h
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
        let max_w = inner_area.width.saturating_sub(6).max(2);
        let x_off = i32::from(inner_area.x + 3);
        let max_w_i32 = i32::from(max_w);

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());
        let mut y = vp_top - self.scroll_y;
        let click_x = i32::from(mouse.x);
        let click_y = i32::from(mouse.y);

        self.ensure_height_caches_fresh(session, max_w, config);

        for (idx, msg) in session.messages.iter().enumerate() {
            if idx > 0 {
                y += 1;
            }

            let msg_h = self.msg_height_cache[idx];
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

                for (pi, part) in msg.parts.iter().enumerate() {
                    let part_h = i32::from(
                        self.part_heights_cache[idx]
                            .get(pi)
                            .copied()
                            .unwrap_or(1)
                            .max(1),
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
                            let part_id = &r.text[..r.text.floor_char_boundary(32)];
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

    pub fn render_message_height(
        msg: &Message,
        max_w: u16,
        config: &TuiConfig,
        part_heights: Option<&[u16]>,
    ) -> i32 {
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
            i32::from(cosh_tui::core::lib::unicode_util::word_wrap(&error_text, max_w).len() as u16)
        } else if let Some(heights) = part_heights {
            i32::from(heights.iter().copied().sum::<u16>())
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

    /// Recompute total_content height from the height cache.
    fn recompute_total_height(&self, session: &crate::types::Session) -> i32 {
        let mut h: i32 = 0;
        for (idx, _m) in session.messages.iter().enumerate() {
            let gap = i32::from(idx > 0);
            h += gap + self.msg_height_cache[idx];
        }
        h
    }

    /// Ensure height caches match the current session. Performs a full rebuild
    /// if terminal width changed or config changed. If only message count grew
    /// (new messages appended), extends the cache incrementally without touching
    /// existing entries. Otherwise, incrementally updates only the last message
    /// if its content changed (streaming — tool status, output, reasoning, text).
    /// Returns `true` if a full rebuild occurred (callers may need to clear
    /// additional caches on rebuild).
    fn ensure_height_caches_fresh(
        &mut self,
        session: &crate::types::Session,
        max_w: u16,
        config: &TuiConfig,
    ) -> bool {
        let config_tok = config_token(config);
        let session_changed = self.last_session_id.as_deref() != Some(session.id.as_str());
        let cache_stale = session_changed
            || self.msg_height_cache.len() != session.messages.len()
            || self.cache_max_w != max_w
            || self.cache_config_token != config_tok;

        if cache_stale {
            let config_or_width_changed =
                self.cache_max_w != max_w || self.cache_config_token != config_tok;
            let count_grew =
                !config_or_width_changed && session.messages.len() > self.msg_height_cache.len();

            if config_or_width_changed || !count_grew || session_changed {
                // Full rebuild: config/width changed, or count decreased
                let _start = Instant::now();
                self.msg_height_cache.clear();
                self.part_heights_cache.clear();
                for m in session.messages.iter() {
                    let part_hs: Vec<u16> = m
                        .parts
                        .iter()
                        .map(|p| Self::estimate_part_height(p, max_w, config, &m.role))
                        .collect();
                    let msg_h = Self::render_message_height(m, max_w, config, Some(&part_hs));
                    self.part_heights_cache.push(part_hs);
                    self.msg_height_cache.push(msg_h);
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.last_session_id = Some(session.id.clone());
                self.cached_total_height = self.recompute_total_height(session);
                log::debug!(
                    "[PERF] msg_height_cache: cold_build={}us msgs={}",
                    _start.elapsed().as_micros(),
                    session.messages.len()
                );
                true
            } else {
                // Extend cache with new messages only (same width/config)
                let prev_len = self.msg_height_cache.len();
                for m in session.messages.iter().skip(prev_len) {
                    let part_hs: Vec<u16> = m
                        .parts
                        .iter()
                        .map(|p| Self::estimate_part_height(p, max_w, config, &m.role))
                        .collect();
                    let msg_h = Self::render_message_height(m, max_w, config, Some(&part_hs));
                    self.part_heights_cache.push(part_hs);
                    self.msg_height_cache.push(msg_h);
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.last_session_id = Some(session.id.clone());
                self.cached_total_height = self.recompute_total_height(session);
                log::debug!(
                    "[PERF] msg_height_cache: extended prev={} now={}",
                    prev_len,
                    session.messages.len()
                );
                false
            }
        } else {
            // Incremental update: refresh only the last message when its
            // content/status changes (streaming token generation, tool completion).
            let current_token = session.messages.last().map(msg_change_token).unwrap_or(0);
            if current_token != self.last_msg_change_token && !self.msg_height_cache.is_empty() {
                let last_idx = session.messages.len() - 1;
                let last_msg = &session.messages[last_idx];
                let part_hs: Vec<u16> = last_msg
                    .parts
                    .iter()
                    .map(|p| Self::estimate_part_height(p, max_w, config, &last_msg.role))
                    .collect();
                let msg_h = Self::render_message_height(last_msg, max_w, config, Some(&part_hs));
                if last_idx < self.part_heights_cache.len() {
                    self.part_heights_cache[last_idx] = part_hs;
                    self.msg_height_cache[last_idx] = msg_h;
                }
                self.last_msg_change_token = current_token;
                self.last_session_id = Some(session.id.clone());
                self.cached_total_height = self.recompute_total_height(session);
                log::debug!("[PERF] msg_height_cache: updated last msg (streaming)");
            } else {
                log::debug!("[PERF] msg_height_cache: hit (cached)");
            }
            false
        }
    }

    #[allow(clippy::too_many_lines, clippy::cast_sign_loss)]
    pub fn build_text_regions(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
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

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            let msg_bottom = y + msg_h;

            // Only process messages that overlap with the viewport
            if msg_bottom > vp_top && msg_top < vp_bottom {
                let border_offset = match msg.role {
                    MessageRole::User => 1,
                    MessageRole::Assistant => 0,
                };
                let mut part_y = msg_top + border_offset;

                for (pi, part) in msg.parts.iter().enumerate() {
                    // No .max(1): hidden parts (e.g. plan_todo_write running/cleared) have
                    // height 0 and must not advance part_y to avoid scroll inconsistencies.
                    let raw_h = self
                        .part_heights_cache
                        .get(idx)
                        .and_then(|ph| ph.get(pi))
                        .copied()
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(part, max_w, config, &msg.role)
                        });
                    let part_h = i32::from(raw_h);
                    let p_top = part_y;
                    let p_bottom = part_y + part_h;

                    // Content-space position of this part's first row
                    let content_offset = p_top - vp_top + scroll;

                    // Only add regions for parts that overlap the viewport
                    if p_bottom > vp_top && p_top < vp_bottom {
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

                                // ── Assistant text: render markdown into temp buffer and scan ──
                                // First tries cached text regions (from the render loop on a previous
                                // frame); falls back to a direct markdown render into a temp buffer.
                                if msg.role == MessageRole::Assistant {
                                    let used_cache = self
                                        .msg_cache_text_regions
                                        .get(idx)
                                        .and_then(|r| r.as_ref())
                                        .is_some_and(|cached| {
                                            let p_start_cs = content_offset;
                                            let p_end_cs = content_offset + part_h;
                                            let vp_start_cs = scroll;
                                            let vp_end_cs = scroll + (vp_bottom - vp_top);
                                            let cs_start = p_start_cs.max(vp_start_cs);
                                            let cs_end = p_end_cs.min(vp_end_cs);
                                            for region in cached {
                                                if region.y1 >= cs_start && region.y1 < cs_end {
                                                    self.text_regions.push(TextRegion {
                                                        y1: region.y1,
                                                        y2: region.y1 + 1,
                                                        x1: x_off,
                                                        x2: x_off + max_w,
                                                        text: region.text.clone(),
                                                    });
                                                }
                                            }
                                            cached.iter().any(|r| r.y1 >= cs_start && r.y1 < cs_end)
                                        });

                                    if !used_cache {
                                        let generous_h =
                                            ((part_h as u16).saturating_mul(3)).clamp(200, 5000);
                                        let scan_area = Rect::new(0, 0, max_w, generous_h);
                                        let mut temp = ratatui::buffer::Buffer::empty(scan_area);

                                        let empty_style = Style::default()
                                            .bg(rgba_color(theme.background))
                                            .fg(rgba_color(theme.text));
                                        for ty in 0..generous_h {
                                            for tx in 0..max_w {
                                                if let Some(cell) = temp.cell_mut((tx, ty)) {
                                                    cell.set_char(' ');
                                                    cell.set_style(empty_style);
                                                }
                                            }
                                        }

                                        let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                                            Some(content.clone()),
                                        );
                                        md.set_fg(Some(ColorInput::RGBA(theme.text)));
                                        md.set_bg(Some(ColorInput::RGBA(theme.background)));
                                        md.set_table_border_color(Some(ColorInput::RGBA(
                                            RGBA::from_ints(255, 200, 0, 255),
                                        )));
                                        md.render_self(&mut temp, scan_area);

                                        let screen_end = p_bottom.min(vp_bottom) as u16;
                                        for (screen_line_y, ty) in
                                            (p_top.max(vp_top) as u16..).zip(0..generous_h)
                                        {
                                            if screen_line_y >= screen_end {
                                                break;
                                            }

                                            let mut line_text = String::new();
                                            for tx in 0..max_w {
                                                if let Some(cell) = temp.cell((tx, ty)) {
                                                    line_text.push(
                                                        cell.symbol().chars().next().unwrap_or(' '),
                                                    );
                                                }
                                            }

                                            let trimmed = line_text.trim_end().to_string();
                                            let cy = (screen_line_y as i32) - vp_top + scroll;
                                            self.text_regions.push(TextRegion {
                                                y1: cy,
                                                y2: cy + 1,
                                                x1: x_off,
                                                x2: x_off + max_w,
                                                text: trimmed,
                                            });
                                        }
                                    }
                                } else {
                                    // ── User text: width-aware wrapping (CJK, emoji, flags = 2 cols) ──
                                    let mut screen_line_y = p_top.max(vp_top) as u16;
                                    let screen_end = p_bottom.min(vp_bottom) as u16;

                                    for logical_line in content.lines() {
                                        if logical_line.is_empty() {
                                            if screen_line_y < screen_end {
                                                let cy = (screen_line_y as i32) - vp_top + scroll;
                                                self.text_regions.push(TextRegion {
                                                    y1: cy,
                                                    y2: cy + 1,
                                                    x1: x_off,
                                                    x2: x_off + max_w,
                                                    text: String::new(),
                                                });
                                            }
                                            continue;
                                        }
                                        let lines = cosh_tui::core::lib::unicode_util::word_wrap(
                                            logical_line,
                                            max_w,
                                        );
                                        for visual_line in &lines {
                                            if screen_line_y >= screen_end {
                                                break;
                                            }
                                            let cy = (screen_line_y as i32) - vp_top + scroll;
                                            self.text_regions.push(TextRegion {
                                                y1: cy,
                                                y2: cy + 1,
                                                x1: x_off,
                                                x2: x_off + max_w,
                                                text: visual_line.clone(),
                                            });
                                            screen_line_y += 1;
                                        }
                                    }
                                }
                            }
                            crate::types::Part::Tool(t) => {
                                // Skip hidden TODO tools entirely (no text regions, no spacing)
                                if crate::util::tool_render::tool_display(&t.tool) == "todo"
                                    && !matches!(t.status, crate::types::ToolStatus::Failed(_))
                                    && (matches!(t.status, crate::types::ToolStatus::Running)
                                        || t.output.as_deref().unwrap_or("").trim().is_empty())
                                {
                                    continue;
                                }
                                if !config.show_tool_details
                                    && matches!(t.status, crate::types::ToolStatus::Completed)
                                {
                                    part_y += part_h;
                                    continue;
                                }
                                if !config.show_generic_tool_output
                                    && crate::util::tool_render::tool_display(&t.tool) == "generic"
                                {
                                    part_y += part_h;
                                    continue;
                                }
                                // Add inline tool label (only when visible)
                                if p_top >= vp_top {
                                    let label = crate::util::tool_render::tool_inline_text(t);
                                    self.text_regions.push(TextRegion {
                                        y1: content_offset,
                                        y2: content_offset + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text: label,
                                    });
                                }
                                // Add visible output lines after the label
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
                                        let first_output_screen = (p_top.max(vp_top) + 1) as u16;
                                        let screen_end = p_bottom.min(vp_bottom) as u16;
                                        let mut out_screen_y = first_output_screen;
                                        for display_line in display.lines() {
                                            if out_screen_y < screen_end {
                                                let cy = (out_screen_y as i32) - vp_top + scroll;
                                                self.text_regions.push(TextRegion {
                                                    y1: cy,
                                                    y2: cy + 1,
                                                    x1: x_off,
                                                    x2: x_off + max_w,
                                                    text: display_line.to_string(),
                                                });
                                                out_screen_y += 1;
                                            }
                                        }
                                    }
                                }
                            }
                            crate::types::Part::Reasoning(r) => {
                                let expanded = config.thinking_mode
                                    || self
                                        .tool_state
                                        .is_expanded(&r.text[..r.text.floor_char_boundary(32)]);
                                let header = if expanded { "- Thought" } else { "+ Thought" };
                                if p_top >= vp_top {
                                    self.text_regions.push(TextRegion {
                                        y1: content_offset,
                                        y2: content_offset + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text: header.to_string(),
                                    });
                                }
                                if expanded && !r.text.is_empty() {
                                    let screen_line_start = (p_top.max(vp_top) + 1) as u16;
                                    let screen_end = p_bottom.min(vp_bottom) as u16;
                                    let truncated =
                                        r.text.lines().take(10).collect::<Vec<_>>().join("\n");
                                    let mut screen_line_y = screen_line_start;
                                    for line in truncated.lines() {
                                        if screen_line_y < screen_end && !line.is_empty() {
                                            let cy = (screen_line_y as i32) - vp_top + scroll;
                                            self.text_regions.push(TextRegion {
                                                y1: cy,
                                                y2: cy + 1,
                                                x1: x_off + 2,
                                                x2: x_off + max_w,
                                                text: line.to_string(),
                                            });
                                            screen_line_y += 1;
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
        // Convert screen-space anchor/focus to content-space coordinates.
        // The anchor was captured at mouse_down_scroll_y; the focus at the
        // current scroll_y.  Content-space allows the selection to survive
        // scroll changes during drag (auto-scroll).
        let vp_top = self.session_area.map_or(0, |(_, y, _, _)| i32::from(y));
        let content_anchor = (anchor_y as i32) - vp_top + self.mouse_down_scroll_y;
        let content_focus = (focus_y as i32) - vp_top + self.scroll_y;

        let start_content_y = content_anchor.min(content_focus);
        let end_content_y = content_anchor.max(content_focus);

        // Determine x-bounds from the content-space drag direction.
        let (start_x, end_x) = if content_anchor <= content_focus {
            (anchor_x, focus_x)
        } else {
            (focus_x, anchor_x)
        };

        let mut result = String::new();
        for region in &self.text_regions {
            if region.y1 > end_content_y || region.y2 <= start_content_y {
                continue;
            }

            let line_y = region.y1;

            let (lx1, lx2) = if start_content_y == end_content_y {
                (start_x.min(end_x), start_x.max(end_x))
            } else if line_y == start_content_y {
                (start_x, region.x2)
            } else if line_y == end_content_y {
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

        self.session_area = Some((area.x, area.y, area.right(), area.bottom()));

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );

        let unique_agents = state.unique_agents();
        let agent_colors = AgentColors::from_theme(theme);
        let max_w = inner_area.width.saturating_sub(6).max(2);
        let streaming = state.status == SessionStatus::Working;

        let _frame_start = Instant::now();

        let config_tok = config_token(config);
        let cache_rebuilt = self.ensure_height_caches_fresh(session, max_w, config);

        // Ensure render cache vectors match message count
        let n_msgs = session.messages.len();
        if cache_rebuilt {
            self.msg_cache_tokens.clear();
            self.msg_cache_cells.clear();
            self.msg_cache_w.clear();
            self.msg_cache_h.clear();
            self.msg_cache_text_regions.clear();
        }
        self.msg_cache_tokens.resize(n_msgs, !0);
        self.msg_cache_cells.resize(n_msgs, None);
        self.msg_cache_w.resize(n_msgs, 0);
        self.msg_cache_h.resize(n_msgs, 0);
        self.msg_cache_text_regions.resize(n_msgs, None);

        let total_height = self.cached_total_height;
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

        let regions_gen = text_regions_generation(session, config, max_w);
        if regions_gen != self.text_regions_gen {
            let _regions_start = Instant::now();
            self.build_text_regions(session, inner_area, max_w, config, theme);
            self.text_regions_gen = regions_gen;
            log::debug!(
                "[PERF] text_regions: {}us (built)",
                _regions_start.elapsed().as_micros()
            );
        } else {
            log::debug!("[PERF] text_regions: skipped (no change)");
        }

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

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            let msg_bottom = y + msg_h;
            let is_assistant_non_error =
                matches!(msg.role, MessageRole::Assistant) && !msg.id.starts_with("msg-err-");
            let mut render_actual_h = msg_h;

            // Check if message overlaps with viewport (using i32, no u16 wrap)
            if msg_bottom > vp_top && msg_top < vp_bottom {
                let is_top_clipped = msg_top < vp_top;

                if is_top_clipped && is_assistant_non_error {
                    // ── Top-clipped assistant (non-error) ──
                    let is_streaming_msg = idx == session.messages.len() - 1 && streaming;
                    let token = msg_content_token(msg, config_tok, max_w);
                    let cache_hit = !is_streaming_msg
                        && token == self.msg_cache_tokens[idx]
                        && self.msg_cache_w[idx] == inner_area.width
                        && self.msg_cache_h[idx] > 0
                        && self.msg_cache_cells[idx].is_some();

                    if cache_hit {
                        // ── Use cached cells ──
                        let cached_cells = self.msg_cache_cells[idx].as_ref().unwrap();
                        let cached_h = self.msg_cache_h[idx];
                        let cached_w = self.msg_cache_w[idx] as usize;
                        let src_y = (vp_top - msg_top) as u16;
                        let dst_y = vp_top as u16;
                        let vis_h = ((cached_h as i32).min(vp_bottom - msg_top) - src_y as i32)
                            .max(0) as u16;
                        for dy in 0..vis_h {
                            let base = (src_y + dy) as usize * cached_w;
                            for dx in 0..cached_w {
                                if let Some(dst) =
                                    buf.cell_mut((inner_area.x + dx as u16, dst_y + dy))
                                {
                                    *dst = cached_cells[base + dx].clone();
                                }
                            }
                        }
                        render_actual_h = cached_h as i32;
                    } else {
                        // ── Fall back: temp buffer render ──
                        let src_y = (vp_top - msg_top) as u16;
                        let dst_y = vp_top as u16;
                        let generous_h =
                            ((msg_h as u16).saturating_add(inner_area.height)).clamp(100, 5000);
                        let full_area = Rect::new(0, 0, inner_area.width, generous_h);
                        let mut temp = Buffer::empty(full_area);
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

                        let mut actual_h = Self::render_assistant_message(
                            &mut temp,
                            full_area,
                            msg,
                            theme,
                            &self.tool_state,
                            config,
                            false,
                            false,
                            &self.part_heights_cache[idx],
                            streaming,
                        ) as i32;
                        actual_h = actual_h.min(generous_h as i32);

                        let msg_content_top = msg_top - vp_top + self.scroll_y;
                        if actual_h > 0 {
                            let x_off_text = inner_area.x + 3;
                            let temp_cells = temp.content();
                            let total_stride = inner_area.width as usize;
                            let content_w = (max_w as usize).min(total_stride.saturating_sub(3));
                            let ah = actual_h as u16;
                            let mut content_cells = Vec::with_capacity(content_w * ah as usize);
                            for dy in 0..ah as usize {
                                let base = dy * total_stride;
                                for dx in 0..content_w {
                                    content_cells.push(temp_cells[base + 3 + dx].clone());
                                }
                            }
                            let regions = Self::cells_to_text_regions(
                                &content_cells,
                                content_w,
                                ah,
                                msg_content_top,
                                x_off_text,
                                max_w,
                            );
                            self.msg_cache_text_regions[idx] = Some(regions);
                        }

                        let vis_h =
                            (actual_h.min(vp_bottom - msg_top) - src_y as i32).max(0) as u16;
                        let temp_cells = temp.content();
                        let total_stride = inner_area.width as usize;
                        let dst_x = inner_area.x;
                        for dy in 0..vis_h {
                            let temp_y = src_y + dy;
                            let dst_line_y = dst_y + dy;
                            let base = temp_y as usize * total_stride;
                            for dx in 0..total_stride {
                                if let Some(dst) = buf.cell_mut((dst_x + dx as u16, dst_line_y)) {
                                    *dst = temp_cells[base + dx].clone();
                                }
                            }
                        }

                        // Save non-streaming messages to cache
                        if !is_streaming_msg && actual_h > 0 {
                            let ah = actual_h as u16;
                            let temp_cells = temp.content();
                            let total_stride = inner_area.width as usize;
                            // Store full-width cells (including border/margin) for consistent
                            // alignment regardless of scroll state.
                            let mut cells = Vec::with_capacity(total_stride * ah as usize);
                            for dy in 0..ah {
                                let base = dy as usize * total_stride;
                                for dx in 0..total_stride {
                                    cells.push(temp_cells[base + dx].clone());
                                }
                            }
                            // Text regions are still content-only (no border/margin text).
                            let x_off_text = inner_area.x + 3;
                            let msg_content_top = msg_top - vp_top + self.scroll_y;
                            let content_w = (max_w as usize).min(total_stride.saturating_sub(3));
                            let mut text_cells = Vec::with_capacity(content_w * ah as usize);
                            for dy in 0..ah {
                                let base = dy as usize * total_stride;
                                for dx in 0..content_w {
                                    text_cells.push(temp_cells[base + 3 + dx].clone());
                                }
                            }
                            let regions = Self::cells_to_text_regions(
                                &text_cells,
                                content_w,
                                ah,
                                msg_content_top,
                                x_off_text,
                                max_w,
                            );
                            self.msg_cache_tokens[idx] = token;
                            self.msg_cache_w[idx] = inner_area.width;
                            self.msg_cache_h[idx] = ah;
                            self.msg_cache_cells[idx] = Some(cells);
                            self.msg_cache_text_regions[idx] = Some(regions);
                        }

                        render_actual_h = actual_h.max(1);
                    }
                } else if is_top_clipped {
                    // ── Top-clipped user/error: fixed-size temp buffer ──
                    let src_y = (vp_top - msg_top) as u16;
                    let dst_y = vp_top as u16;
                    let vis_h = (msg_bottom.min(vp_bottom) - vp_top) as u16;

                    if vis_h > 0 && msg_h > 0 {
                        let full_area = Rect::new(0, 0, inner_area.width, msg_h as u16);
                        let mut temp = Buffer::empty(full_area);
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

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
                                    Some(&self.part_heights_cache[idx]),
                                    streaming,
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
                                    &self.part_heights_cache[idx],
                                    streaming,
                                );
                            }
                        }

                        let temp_cells = temp.content();
                        let line_stride = full_area.width as usize;
                        let dst_x = inner_area.x;
                        for dy in 0..vis_h {
                            let temp_y = src_y + dy;
                            let dst_line_y = dst_y + dy;
                            let base = temp_y as usize * line_stride;
                            for dx in 0..line_stride {
                                if let Some(dst) = buf.cell_mut((dst_x + dx as u16, dst_line_y)) {
                                    *dst = temp_cells[base + dx].clone();
                                }
                            }
                        }
                    }
                } else if is_assistant_non_error && msg_bottom <= vp_bottom {
                    // ── Fully visible assistant: render directly, use actual height ──
                    let visible_top = msg_top as u16;
                    let visible_bottom = msg_bottom.min(vp_bottom);
                    let visible_h = (visible_bottom - i32::from(visible_top)) as u16;

                    if visible_h > 0 {
                        let is_streaming_msg = idx == session.messages.len() - 1 && streaming;
                        let token = msg_content_token(msg, config_tok, max_w);
                        let cache_hit = !is_streaming_msg
                            && token == self.msg_cache_tokens[idx]
                            && self.msg_cache_w[idx] == inner_area.width
                            && self.msg_cache_h[idx] > 0;

                        if cache_hit {
                            if let Some(ref cached_cells) = self.msg_cache_cells[idx] {
                                let w = self.msg_cache_w[idx] as usize;
                                let h = self.msg_cache_h[idx];
                                for dy in 0..visible_h.min(h) {
                                    let base = dy as usize * w;
                                    for dx in 0..w {
                                        if let Some(cell) = buf
                                            .cell_mut((inner_area.x + dx as u16, visible_top + dy))
                                        {
                                            *cell = cached_cells[base + dx].clone();
                                        }
                                    }
                                }
                                render_actual_h = h as i32;

                                // Ensure text regions are cached for this message
                                if self.msg_cache_text_regions[idx].is_none() {
                                    let x_off_text = inner_area.x + 3;
                                    let msg_content_top = msg_top - vp_top + self.scroll_y;
                                    let regions = Self::cells_to_text_regions(
                                        cached_cells,
                                        w,
                                        h,
                                        msg_content_top,
                                        x_off_text,
                                        max_w,
                                    );
                                    self.msg_cache_text_regions[idx] = Some(regions);
                                }
                            }
                        } else {
                            let msg_area =
                                Rect::new(inner_area.x, visible_top, inner_area.width, visible_h);

                            let actual_h = Self::render_assistant_message(
                                buf,
                                msg_area,
                                msg,
                                theme,
                                &self.tool_state,
                                config,
                                false,
                                false,
                                &self.part_heights_cache[idx],
                                streaming,
                            ) as i32;

                            render_actual_h = actual_h.max(1);

                            // Save non-streaming messages to cache (cells + text regions)
                            if !is_streaming_msg {
                                let ah = render_actual_h as u16;
                                let w = inner_area.width as usize;
                                let x_off_text = inner_area.x + 3;
                                let msg_content_top = msg_top - vp_top + self.scroll_y;
                                let mut cells = Vec::with_capacity(w * ah as usize);
                                for dy in 0..ah {
                                    for dx in 0..w {
                                        let c = buf
                                            .cell((inner_area.x + dx as u16, visible_top + dy))
                                            .cloned()
                                            .unwrap_or_default();
                                        cells.push(c);
                                    }
                                }
                                let regions = Self::cells_to_text_regions(
                                    &cells,
                                    w,
                                    ah,
                                    msg_content_top,
                                    x_off_text,
                                    max_w,
                                );
                                self.msg_cache_tokens[idx] = token;
                                self.msg_cache_w[idx] = inner_area.width;
                                self.msg_cache_h[idx] = ah;
                                self.msg_cache_cells[idx] = Some(cells);
                                self.msg_cache_text_regions[idx] = Some(regions);
                            }
                        }
                    }
                } else {
                    // ── Not top-clipped user/error or bottom-clipped: render directly ──
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
                                    Some(&self.part_heights_cache[idx]),
                                    streaming,
                                );
                            }
                            MessageRole::Assistant => {
                                let is_streaming_msg =
                                    idx == session.messages.len() - 1 && streaming;
                                let token = msg_content_token(msg, config_tok, max_w);
                                let cache_hit = !is_streaming_msg
                                    && token == self.msg_cache_tokens[idx]
                                    && self.msg_cache_w[idx] == inner_area.width
                                    && self.msg_cache_h[idx] > 0
                                    && self.msg_cache_cells[idx].is_some();

                                if cache_hit {
                                    let cached_cells = self.msg_cache_cells[idx].as_ref().unwrap();
                                    let cached_h = self.msg_cache_h[idx];
                                    let cached_w = self.msg_cache_w[idx] as usize;
                                    for dy in 0..visible_h.min(cached_h) {
                                        let base = dy as usize * cached_w;
                                        for dx in 0..cached_w {
                                            if let Some(cell) = buf.cell_mut((
                                                inner_area.x + dx as u16,
                                                visible_top + dy,
                                            )) {
                                                *cell = cached_cells[base + dx].clone();
                                            }
                                        }
                                    }
                                    render_actual_h = cached_h as i32;
                                } else if msg.id.starts_with("msg-err-") {
                                    // Error messages: render directly to buf.
                                    // render_assistant_message returns 0 for errors
                                    // (content is rendered as a side-effect), so the
                                    // temp buffer approach would lose the output.
                                    Self::render_assistant_message(
                                        buf,
                                        msg_area,
                                        msg,
                                        theme,
                                        &self.tool_state,
                                        config,
                                        false,
                                        false,
                                        &self.part_heights_cache[idx],
                                        streaming,
                                    );
                                    render_actual_h = msg_h;
                                } else {
                                    // Render full message to temp buffer, then copy
                                    // visible portion to buf. This ensures the full
                                    // message height is cached, not just the clipped
                                    // viewport portion — preventing partial cut-off
                                    // on subsequent frames (e.g. after theme change).
                                    let generous_h = ((msg_h as u16)
                                        .saturating_add(inner_area.height))
                                    .clamp(100, 5000);
                                    let full_area = Rect::new(0, 0, inner_area.width, generous_h);
                                    let mut temp = Buffer::empty(full_area);
                                    temp.set_style(
                                        full_area,
                                        Style::default().bg(rgba_color(theme.background)),
                                    );

                                    let mut actual_h = Self::render_assistant_message(
                                        &mut temp,
                                        full_area,
                                        msg,
                                        theme,
                                        &self.tool_state,
                                        config,
                                        false,
                                        false,
                                        &self.part_heights_cache[idx],
                                        streaming,
                                    ) as i32;
                                    actual_h = actual_h.min(generous_h as i32);

                                    let msg_content_top = msg_top - vp_top + self.scroll_y;
                                    if actual_h > 0 {
                                        let x_off_text = inner_area.x + 3;
                                        let temp_cells = temp.content();
                                        let total_stride = inner_area.width as usize;
                                        let content_w =
                                            (max_w as usize).min(total_stride.saturating_sub(3));
                                        let ah = actual_h as u16;
                                        let mut content_cells =
                                            Vec::with_capacity(content_w * ah as usize);
                                        for dy in 0..ah as usize {
                                            let base = dy * total_stride;
                                            for dx in 0..content_w {
                                                content_cells
                                                    .push(temp_cells[base + 3 + dx].clone());
                                            }
                                        }
                                        let regions = Self::cells_to_text_regions(
                                            &content_cells,
                                            content_w,
                                            ah,
                                            msg_content_top,
                                            x_off_text,
                                            max_w,
                                        );
                                        self.msg_cache_text_regions[idx] = Some(regions);
                                    }

                                    // Copy visible portion from temp to buf
                                    let dst_y = visible_top;
                                    let vis_h = actual_h.min(i32::from(visible_h)) as u16;
                                    let temp_cells = temp.content();
                                    let total_stride = inner_area.width as usize;
                                    let dst_x = inner_area.x;
                                    for dy in 0..vis_h {
                                        let base = dy as usize * total_stride;
                                        let dst_line_y = dst_y + dy;
                                        for dx in 0..total_stride {
                                            if let Some(dst) =
                                                buf.cell_mut((dst_x + dx as u16, dst_line_y))
                                            {
                                                *dst = temp_cells[base + dx].clone();
                                            }
                                        }
                                    }

                                    render_actual_h = actual_h.max(1);

                                    // Save non-streaming messages to cache (full height)
                                    if !is_streaming_msg && actual_h > 0 {
                                        let ah = render_actual_h as u16;
                                        let w = inner_area.width as usize;
                                        let mut cells = Vec::with_capacity(w * ah as usize);
                                        for dy in 0..ah {
                                            let base = dy as usize * total_stride;
                                            for dx in 0..w {
                                                cells.push(temp_cells[base + dx].clone());
                                            }
                                        }
                                        let x_off_text = inner_area.x + 3;
                                        let regions = Self::cells_to_text_regions(
                                            &cells,
                                            w,
                                            ah,
                                            msg_content_top,
                                            x_off_text,
                                            max_w,
                                        );
                                        self.msg_cache_tokens[idx] = token;
                                        self.msg_cache_w[idx] = inner_area.width;
                                        self.msg_cache_h[idx] = ah;
                                        self.msg_cache_cells[idx] = Some(cells);
                                        self.msg_cache_text_regions[idx] = Some(regions);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            y += render_actual_h;
        }

        // Sync cached total height with actual rendered height.
        // The actual rendered total (from the render loop's y-advancement)
        // can exceed the cached estimate when `scan_content_height` returns
        // more rows than `estimate_height` predicted (due to the generous
        // `(est_h + 5).max(10)` allocation in render_parts for assistant text).
        // Without this correction, scroll_y is clamped to max_scroll based on
        // the underestimated cached_total, cutting off the last message.
        let actual_total = y - (vp_top - self.scroll_y);
        self.cached_total_height = self.cached_total_height.max(actual_total);
        self.total_height = self.cached_total_height;

        if let Some((anchor_x, _anchor_screen_y, focus_x, _focus_screen_y)) = self.drag_selection {
            // Convert content-space anchor and focus to current screen position.
            // Both are stored in content space so the visual highlight follows
            // content during auto-scroll.
            let vp_top = i32::from(inner_area.y);
            let anchor_screen_y = (self.selection_anchor_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_top + i32::from(inner_area.height) - 1)
                as u16;
            let focus_screen_y = (self.selection_focus_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_top + i32::from(inner_area.height) - 1)
                as u16;

            let start_y = anchor_screen_y.min(focus_screen_y);
            let end_y = anchor_screen_y.max(focus_screen_y);

            let (start_x, end_x) = if anchor_screen_y == start_y {
                (anchor_x, focus_x)
            } else {
                (focus_x, anchor_x)
            };

            let content_min_x = inner_area.x + 3;
            let content_max_x = (inner_area.x + 3 + max_w).saturating_sub(1);

            for cy in start_y..=end_y {
                let (lx1, lx2) = if start_y == end_y {
                    (start_x.min(end_x), start_x.max(end_x))
                } else if cy == start_y {
                    (start_x, content_max_x)
                } else if cy == end_y {
                    (content_min_x, end_x)
                } else {
                    (content_min_x, content_max_x)
                };

                let lx1 = lx1.max(content_min_x);
                let lx2 = lx2.min(content_max_x);
                if lx1 > lx2 {
                    continue;
                }

                for cx in lx1..=lx2 {
                    if let Some(cell) = buf.cell_mut((cx, cy)) {
                        let fg = cell.fg;
                        let bg = cell.bg;
                        cell.set_fg(bg);
                        cell.set_bg(fg);
                    }
                }
            }
        }

        self.handle_auto_scroll(delta_time, total_height, visible_height);

        let _frame_us = _frame_start.elapsed().as_micros();
        if _frame_us > 5000 {
            log::debug!(
                "[PERF] session_render_total: {_frame_us}us msgs={} scroll_y={}",
                session.messages.len(),
                self.scroll_y
            );
        }

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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
