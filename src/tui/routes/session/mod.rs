pub mod footer;
pub mod free_gateway_recommendation;
pub mod streaming;
pub mod tool_render;

pub mod dashboard;
pub mod delete;
pub mod permission;
pub mod question;
pub mod queue_choice;
pub mod right_panel;
pub mod sidebar;
pub mod subagent_footer;
#[cfg(test)]
mod tests;

use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::{ColorInput, RGBA, ansi256_index_to_rgb};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::scroll_bar::{ScrollBarOrientation, ScrollBarRenderable};
use cosh_tui::core::types::MouseEvent;

use cosh_tui::core::renderables::markdown::estimate_height;

use std::hash::Hasher;

use self::tool_render::ToolRenderState;
use crate::config::TuiConfig;
use crate::state::AppState;
use crate::theme::{Theme, rgba_color};
use crate::types::{
    AgentColors, CompactionPart, FilePart, Message, MessageRole, Part, ReasoningPart,
    SessionStatus, ToolPart, ToolStatus,
};
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

/// Draws the expand/collapse hint styled as a button: highlighted with the
/// theme primary color as a marker-like background so it reads as clickable.
fn draw_hint_button(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, theme: &Theme) {
    let style = Style::default()
        .fg(rgba_color(theme.background))
        .bg(rgba_color(theme.primary));
    let width = (text.chars().count() as u16).saturating_add(2);
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    let right = right.min(x.saturating_add(width));
    for cx in x..right {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_style(style);
        }
    }
    let inner_x = x.saturating_add(1);
    let inner_max_w = max_w.saturating_sub(2);
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let Some(cx) = inner_x.checked_add(i as u16) else {
            break;
        };
        if cx >= inner_x.saturating_add(inner_max_w) {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters: writing them into buffer cells makes
        // ratatui's buffer diff panic ("control character passed to
        // cell_width without filtering").
        if ch.is_control() {
            continue;
        }
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
                if let Some(notes) = &t.lsp_notes {
                    for note in notes.errors.iter().chain(notes.warnings.iter()) {
                        hasher.write(note.path.as_bytes());
                        hasher.write(&note.line.to_le_bytes());
                        hasher.write(note.message.as_bytes());
                    }
                }
            }
            Part::Reasoning(r) => {
                hasher.write(r.text.as_bytes());
            }
            Part::File(f) => {
                hasher.write(f.filename.as_bytes());
                hasher.write(f.mime.as_bytes());
            }
            Part::Compaction(c) => {
                // Hash the full state so the line re-renders exactly once when
                // it finalizes (elapsed_ms transitions None → Some), and so a
                // streamed "Summarizing" body (phase Llm text) invalidates the
                // cache as tokens arrive. While the stopwatch alone ticks, the
                // hash stays stable — the ticking bypasses the cell cache via
                // `msg_has_running_compaction`.
                hasher.write(&[0u8]);
                hasher.write(&c.started_at.to_le_bytes());
                hasher.write(c.text.as_bytes());
                match c.elapsed_ms {
                    Some(ms) => {
                        hasher.write(&[1]);
                        hasher.write(&ms.to_le_bytes());
                    }
                    None => hasher.write(&[0]),
                }
            }
        }
    }
    hasher.finish()
}

/// All compaction phases use muted gray to indicate system messages.
fn compaction_color(theme: &Theme) -> RGBA {
    theme.text_muted
}

/// The chat line text for the LLM "Summarizing" box. `now` is the current
/// wall-clock millis: while the stopwatch line is running the elapsed number
/// ticks every frame (the moving number IS the activity signal — no spinner
/// needed); when it finalizes the same line freezes.
fn compaction_line(part: &CompactionPart, now: u64) -> String {
    let elapsed = part
        .elapsed_ms
        .unwrap_or_else(|| now.saturating_sub(part.started_at));
    // Millisecond precision — the second counter flips visibly.
    let secs = elapsed as f64 / 1000.0;
    format!("llm compaction · {secs:.3}s")
}

/// Max body lines of the COLLAPSED "Summarizing" box. When the streamed
/// summary exceeds it, only the LAST lines stay visible — the top lines
/// visually leave the box (an LRU-like scroll-up effect, nothing is actually
/// removed). Expanded, the box grows to fit the whole text.
const SUMMARIZING_COLLAPSED_LINES: u16 = 8;

/// Vertical padding inside the box: 1 blank row above the title and 1 below
/// the last content row, so the content never touches the box edges.
const SUMMARIZING_PAD_V: u16 = 1;

/// Generous in-memory budget for the per-message render cache (cells +
/// text regions). Once the total exceeds this, the oldest entries outside a
/// guard around the viewport are evicted LRU-style — the transcript on disk
/// keeps the full session, so scroll-back simply re-renders the evicted
/// message. Bounding this cache keeps the TUI fluid on very long sessions.
const RENDER_CACHE_BUDGET: usize = 64 * 1024 * 1024; // 64 MB

/// Eviction stops once the cache is back under this water mark, so a small
/// scroll doesn't immediately re-trigger eviction (anti-thrash).
const RENDER_CACHE_LOW_WATER: usize = 48 * 1024 * 1024; // 48 MB

/// Row margin (in content rows) above/below the viewport that is never
/// evicted — ordinary scrolling within a few screens stays re-render-free.
const RENDER_CACHE_GUARD_ROWS: i32 = 400;

/// Stable expand/collapse key for a "Summarizing" box: one per LLM-compaction
/// line, keyed by its `started_at`.
fn summarizing_id(part: &CompactionPart) -> String {
    format!("summarize-{}", part.started_at)
}

/// The rendered height of the "Summarizing" box. The body is laid out with the
/// SAME markdown algorithm as the render ([`estimate_height`], pulldown_cmark),
/// so the allocated height always matches the drawn body exactly. Collapsed: a
/// fixed preview of the LAST [`SUMMARIZING_COLLAPSED_LINES`] rows plus a hint
/// row when the body overflows; expanded: the whole markdown body. Every case
/// adds [`SUMMARIZING_PAD_V`] rows of padding above the title and below the
/// content.
fn summarizing_height(part: &CompactionPart, expanded: bool, max_w: u16) -> u16 {
    let wrap_w = max_w.saturating_sub(3).max(1);
    let body_h = estimate_height(&part.text, wrap_w).max(1);
    let pad = SUMMARIZING_PAD_V * 2;
    if expanded {
        body_h.saturating_add(1).saturating_add(pad) // pad + title + full body + pad
    } else if body_h > SUMMARIZING_COLLAPSED_LINES {
        SUMMARIZING_COLLAPSED_LINES
            .saturating_add(2)
            .saturating_add(pad) // pad + title + preview + hint + pad
    } else {
        body_h.saturating_add(1).saturating_add(pad)
    }
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

/// True when the message contains a still-running compaction line — its
/// stopwatch ticks every frame, so the per-message cell cache must be
/// bypassed (the stored part data does not change while running, only the
/// rendered elapsed time does).
fn msg_has_running_compaction(msg: &Message) -> bool {
    msg.parts
        .iter()
        .any(|p| matches!(p, Part::Compaction(c) if c.is_running()))
}

/// Indices of messages that can still change in place, so their change token
/// genuinely needs re-checking: those holding a `Running` tool (its output
/// grows / its status transitions) or a `Running` compaction (it finalizes
/// once). Every other message is immutable once set, so re-hashing its full
/// content every frame would pay O(whole transcript) per frame for nothing.
/// The scan is a cheap O(parts) status pass — no content hashing.
fn mutable_msg_indices(session: &crate::types::Session) -> Vec<usize> {
    session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.parts.iter().any(|p| {
                matches!(p, Part::Tool(t) if matches!(t.status, ToolStatus::Running))
                    || matches!(p, Part::Compaction(c) if c.is_running())
            })
        })
        .map(|(i, _)| i)
        .collect()
}

fn msg_content_token(
    msg: &Message,
    config_token: u64,
    max_w: u16,
    tool_state: &ToolRenderState,
) -> u64 {
    let mut h = hash_parts(msg);
    // Expansion toggles must invalidate the per-message render cache. The
    // `version` counter is incremented on every expand/collapse toggle, so
    // hashing it (O(1)) is sufficient — `ensure_height_caches_fresh` already
    // forces a full cache rebuild (clearing `msg_cache_tokens`) whenever the
    // version changes, so the per-part expansion state never needs to be
    // hashed individually.
    h = h.wrapping_mul(31).wrapping_add(tool_state.version);
    h = h.wrapping_mul(31).wrapping_add(config_token);
    h = h.wrapping_mul(31).wrapping_add(max_w as u64);
    h
}

use crate::util::text_region::{TextRegion, extract_text_in_region};

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
    /// Actual scanned content height from last render (used by scrollbar for accurate sizing).
    actual_total_height: i32,

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
    /// Prefix sum of message start positions in content space:
    /// `prefix_y[i]` = the content row where message `i` starts (gap of 1 row
    /// between messages). `prefix_y[n]` = the total content height. Kept in
    /// sync with `msg_height_cache` so `find_first_visible` can locate the
    /// viewport's first message in O(log n) instead of walking the whole
    /// timeline every frame (the O(n) per-frame walk that dominated frame
    /// cost on 500+ message sessions).
    prefix_y: Vec<i32>,
    part_heights_cache: Vec<Vec<u16>>,
    cache_max_w: u16,
    cache_config_token: u64,
    /// Change-detection token of the last message when caches were last built.
    /// Used to detect streaming/tool-status changes without a full cache rebuild.
    last_msg_change_token: u64,
    /// Change-detection token per message (parallel to `msg_height_cache`).
    /// The incremental path re-estimates ANY message whose token changed —
    /// not just the last — so a tool box completing/outputting in an older
    /// message (ToolResult/ToolOutput search every message) keeps its cached
    /// height in sync with what the render actually draws.
    msg_change_tokens: Vec<u64>,
    /// Indices that held a still-mutable part (Running tool / Running
    /// compaction) during the PREVIOUS incremental height pass. Carried over
    /// and re-checked one more pass so a tool that completes in place
    /// (Running → Completed) between frames still lands its final height, then
    /// it drops out on its own. This lets the incremental scan re-hash only the
    /// few messages that can actually change instead of the whole transcript.
    prev_mutable_msgs: Vec<usize>,
    /// ID of the session for which caches were last built.
    /// Forces a full rebuild when switching sessions with the same message count.
    last_session_id: Option<String>,
    /// Index of the user message currently hovered by the mouse cursor, used
    /// for the opencode-style hover highlight and click-to-open "Message
    /// Actions". `None` when the cursor is outside any user message.
    pub hovered_msg_idx: Option<usize>,
    /// Message id captured when the user clicks a user message; app.rs reads
    /// it after `handle_mouse` returns to open the Message Actions dialog.
    pub pending_message_action: Option<String>,
    /// Expansion version of `tool_state` when the height caches were last built.
    /// A change forces a full rebuild so expanded/collapsed heights stay in sync.
    last_tool_state_version: u64,

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
    /// Reusable scratch buffer for temp renders (up to 5000 rows tall). Kept
    /// across frames — `Buffer::resize` retains the underlying allocation, so
    /// streaming no longer allocates/frees a large buffer every frame, which
    /// fragmented the heap and degraded the TUI over the process lifetime.
    scratch: Option<ratatui::buffer::Buffer>,
    /// Monotonic frame counter — recency stamp for the render-cache LRU.
    render_frame: u64,
    /// Last frame each message's cached cells/regions were used (hit or
    /// stored). `prune_render_cache` evicts the oldest stamps first.
    msg_cache_last_used: Vec<u64>,
    /// Approximate live bytes held by `msg_cache_cells` + `msg_cache_text_regions`.
    msg_cache_bytes: usize,
    /// Persistent render of the streaming last message: stable non-text parts
    /// rendered once, the streaming text part patched incrementally (see
    /// `streaming` module). Freed when nothing is streaming.
    streaming_msg: Option<streaming::StreamingMessageCache>,
}

#[derive(Clone, Copy, Default)]
struct HeightCacheUpdate {
    full_rebuild: bool,
    tool_state_changed: bool,
}

fn text_regions_generation(
    session: &crate::types::Session,
    config: &TuiConfig,
    max_w: u16,
    tool_state_version: u64,
    stream_token: u64,
) -> u64 {
    let mut g: u64 = session.messages.len() as u64;
    g = g.wrapping_mul(31).wrapping_add(max_w as u64);
    g = g.wrapping_mul(31).wrapping_add(config_token(config));
    g = g.wrapping_mul(31).wrapping_add(tool_state_version);
    // NOTE: the previous code folded `msg_change_token(last)` (a full content
    // hash of the last message) into this generation. For a long streamed
    // message that hash is O(n) and ran on EVERY frame — even after streaming
    // finished — re-parsing nothing but still forcing `build_text_regions` to
    // run every frame. The streaming cache now keeps the streaming message's
    // regions fresh incrementally, so the caller passes a cheap O(1) token
    // that changes only while the message actually grows.
    g = g.wrapping_mul(31).wrapping_add(stream_token);
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
    if config.diagnostics_mode {
        token |= 16;
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
            actual_total_height: 0,
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
            prefix_y: Vec::new(),
            part_heights_cache: Vec::new(),
            cache_max_w: 0,
            cache_config_token: 0,
            last_msg_change_token: 0,
            msg_change_tokens: Vec::new(),
            prev_mutable_msgs: Vec::new(),
            text_regions_gen: 0,
            msg_cache_tokens: Vec::new(),
            msg_cache_cells: Vec::new(),
            msg_cache_w: Vec::new(),
            msg_cache_h: Vec::new(),
            msg_cache_text_regions: Vec::new(),
            scratch: None,
            render_frame: 0,
            msg_cache_last_used: Vec::new(),
            msg_cache_bytes: 0,
            streaming_msg: None,
            hovered_msg_idx: None,
            pending_message_action: None,
            last_session_id: None,
            last_tool_state_version: 0,
        }
    }

    // ── Scroll control (port of OpenCode's ScrollBox) ──────────────────────────

    /// Accelerated scroll (for mouse wheel). Multiplies delta by `scroll_accel`.
    /// Uses fractional accumulator for smooth scrolling.
    /// Mirrors OpenCode's `onMouseEvent` for scroll type.
    pub fn scroll_by(&mut self, delta: f64) {
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);

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
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);

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
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);
        self.scroll_y = position.clamp(0, max_scroll);
        self.scroll_accumulator_y = 0.0;
        self.sync_manual_scroll_state();
    }

    /// Scroll to bottom. Mirrors OpenCode's `toBottom()`:
    ///   `scroll.scrollTo(scroll.scrollHeight)`
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
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

        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
        let has_scrollable_content = max_scroll > 1;

        self.has_manual_scroll = has_scrollable_content && !self.is_at_sticky_position();

        self.update_sticky_state();
    }

    /// Update sticky state flags. Mirrors OpenCode's `updateStickyState()`.
    fn update_sticky_state(&mut self) {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);

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
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);

        // stickyStart = "bottom"
        if max_scroll <= 0 {
            return true;
        }
        self.scroll_y >= max_scroll
    }

    /// Check if at sticky re-engage point. Mirrors OpenCode's `isAtStickyReengagePoint()`.
    /// For "bottom": `maxScrollTop > 0 && scrollTop >= maxScrollTop - 1`
    pub fn is_at_bottom(&self) -> bool {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
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

        // Scroll to the cached bottom, not the actual scanned bottom,
        // to prevent scroll jumping during streaming. The cached height
        // is the stable source of truth for layout calculations.
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
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

        // Use cached_total_height for max_scroll during streaming to prevent
        // scroll jumping. The cached height is the stable source of truth for
        // layout calculations, while actual_total_height is only used for
        // scrollbar sizing and may fluctuate slightly during rendering.
        let new_max_scroll = (total_height - visible_height).max(0);

        if !self.has_manual_scroll {
            // No manual scroll → apply sticky start
            self.scroll_y = new_max_scroll;
            self.is_sticky_bottom = true;
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

    /// Render the "Summarizing" box of the LLM compaction (the last-resort
    /// fallback): a
    /// bordered title row with a live/elapsed stopwatch, and the streamed
    /// summary body. Collapsed, the body is a fixed-height preview that shows
    /// only the LAST lines (the top lines visually leave the box as the stream
    /// grows — an LRU-like scroll-up; nothing is actually removed). Expanded,
    /// the box grows to fit the whole text. Returns the rendered height.
    #[allow(clippy::too_many_arguments)]
    fn render_summarizing_box(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        part: &CompactionPart,
        expanded: bool,
        theme: &Theme,
    ) -> u16 {
        let now = crate::types::now_ms();
        let elapsed = part
            .elapsed_ms
            .unwrap_or_else(|| now.saturating_sub(part.started_at));
        let secs = elapsed as f64 / 1000.0;
        let wrap_w = max_w.saturating_sub(3).max(1);
        let total_h = summarizing_height(part, expanded, max_w);
        let area = Rect::new(x, y, max_w.saturating_add(3), total_h);

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
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
        });
        border_box.render_self(buf, area);

        let x_off = x + 3;
        // 1 blank row of padding above the title (the bottom pad is the last
        // row of the box); matches SUMMARIZING_PAD_V in summarizing_height.
        let title_y = y + SUMMARIZING_PAD_V;
        // `+`/`-` match the adjacent Thought block's expand/collapse affordance.
        let mut title = if expanded { "- " } else { "+ " }.to_string();
        title.push_str(&format!("Summarizing · {secs:.3}s"));
        let title_style = Style::default().fg(rgba_color(theme.secondary));
        draw_text_line(
            buf,
            &title,
            x_off,
            title_y,
            max_w.saturating_sub(3),
            title_style,
        );

        // Markdown-rendered body — the same renderer as the chat Text parts,
        // so the raw markdown syntax never reaches the screen. The bg is the
        // box's own `background_panel` (NOT the chat `background`): the border
        // box already fills its whole area with the panel color, so painting
        // the body with the chat background would leave a visible darker
        // strip inside the box (default cosh: #07070A vs #0B0B12).
        let body_h = estimate_height(&part.text, wrap_w).max(1);
        let content = sanitize_text(&part.text);
        let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(content));
        md.set_fg(Some(ColorInput::RGBA(theme.text)));
        md.set_bg(Some(ColorInput::RGBA(theme.background_panel)));
        crate::util::markdown::apply_theme(&mut md, theme);
        let overflow = !expanded && body_h > SUMMARIZING_COLLAPSED_LINES;
        if overflow {
            // Collapsed preview: the box shows the LAST
            // `SUMMARIZING_COLLAPSED_LINES` rows of the laid-out markdown (the
            // top rows leave the view as the stream grows — the LRU/scroll-up
            // effect). Markdown only lays out from the top, so the full body
            // is rendered into a scratch buffer first and only the final rows
            // are blitted into the box.
            let tmp_area = Rect::new(0, 0, wrap_w, body_h);
            let mut tmp = ratatui::buffer::Buffer::empty(tmp_area);
            md.render_self(&mut tmp, tmp_area);
            let src_start = body_h - SUMMARIZING_COLLAPSED_LINES;
            for (i, src_y) in (src_start..body_h).enumerate() {
                let dst_y = y + SUMMARIZING_PAD_V + 1 + i as u16;
                if dst_y >= area.bottom() {
                    break;
                }
                for cx in 0..wrap_w {
                    if let Some(src) = tmp.cell((cx, src_y))
                        && let Some(dst) = buf.cell_mut((x_off + cx, dst_y))
                    {
                        dst.set_symbol(src.symbol());
                        dst.set_style(src.style());
                    }
                }
            }
        } else {
            let body_area = Rect::new(
                x_off,
                y + SUMMARIZING_PAD_V + 1,
                wrap_w,
                body_h.min(total_h.saturating_sub(1 + SUMMARIZING_PAD_V)),
            );
            md.render_self(buf, body_area);
        }
        if overflow {
            let hint_y = y + SUMMARIZING_PAD_V + 1 + SUMMARIZING_COLLAPSED_LINES;
            draw_hint_button(buf, "Click to expand", x_off, hint_y, wrap_w, theme);
        }
        total_h
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
            // The body is markdown (word-wrapped) at width `max_w - 2`; its
            // row count must come from the renderer's own layout algorithm
            // (`estimate_height`), not from the source line count: a long
            // paragraph wraps into several visual rows, and underestimating
            // the height clipped the body on screen and desynced the copy
            // regions from what is displayed.
            let body_w = max_w.saturating_sub(2).max(1);
            // Sanitize before the markdown renderer so control chars can't
            // reach buffer cells (ratatui cell_width panic). `\n` is kept —
            // the markdown renderer handles line breaks itself. The height
            // is measured on the sanitized text so it matches the render.
            let content = sanitize_text(&part.text);
            let md_h = estimate_height(&content, body_w).max(1);
            let md_area = Rect::new(x + 2, y + 1, body_w, md_h);
            let mut md =
                cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(content));
            md.set_fg(Some(ColorInput::RGBA(theme.text_muted)));
            md.set_bg(Some(ColorInput::RGBA(theme.background)));
            crate::util::markdown::apply_theme(&mut md, theme);
            md.render_self(buf, md_area);
            // Dim the whole reasoning body toward the background so it reads as
            // opaque "thinking" text — white becomes gray and syntax-highlight
            // colors get grayed out. Uses the theme's `thinking_opacity`. Cells
            // whose foreground is unset (Reset — how the markdown renderer
            // paints plain paragraphs) are colored from the muted base first,
            // so the plain text also ends up dimmed/gray instead of matching
            // the normal reply's color.
            Self::dim_reasoning_region(
                buf,
                md_area,
                theme.thinking_opacity,
                theme.text_muted,
                theme.background,
            );
            *line_h = 1 + md_h;
        }
    }

    /// Resolve a ratatui [`Color`] to its (r, g, b) channels when possible.
    fn color_to_rgb(c: Color) -> Option<(u8, u8, u8)> {
        match c {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Indexed(i) => Some(ansi256_index_to_rgb(i)),
            _ => None,
        }
    }

    /// Blend a foreground channel toward a background channel by `keep`
    /// (1.0 = keep the fg fully, 0.0 = fully the bg colour).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn blend_channel(fg: u8, bg: u8, keep: f64) -> u8 {
        (f64::from(fg) * keep + f64::from(bg) * (1.0 - keep)).round() as u8
    }

    /// Dim a rendered region toward its background so it looks "pushed back" /
    /// opaque. Used for the reasoning/thinking body so the model's internal
    /// text — including syntax highlighting — appears grayed-out (less bright)
    /// compared to the normal reply. `opacity` comes from the theme's
    /// `thinking_opacity` (1.0 = no dimming). `base_fg` is the fallback colour
    /// used for cells with no explicit foreground (Reset — how plain markdown
    /// paragraphs are painted), which is then also dimmed so the plain text is
    /// visibly gray rather than matching the normal reply.
    fn dim_reasoning_region(
        buf: &mut Buffer,
        area: Rect,
        opacity: f64,
        base_fg: RGBA,
        fallback_bg: RGBA,
    ) {
        let keep = opacity.clamp(0.0, 1.0);
        if keep >= 1.0 {
            return;
        }
        let base = Self::color_to_rgb(rgba_color(base_fg)).unwrap_or((220, 220, 220));
        let fallback = Self::color_to_rgb(rgba_color(fallback_bg)).unwrap_or((0, 0, 0));
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let Some(cell) = buf.cell_mut((x, y)) else {
                    continue;
                };
                let (fr, fg_, fb) = Self::color_to_rgb(cell.fg).unwrap_or(base);
                let (br, bg_, bb) = Self::color_to_rgb(cell.bg).unwrap_or(fallback);
                cell.fg = Color::Rgb(
                    Self::blend_channel(fr, br, keep),
                    Self::blend_channel(fg_, bg_, keep),
                    Self::blend_channel(fb, bb, keep),
                );
            }
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
        crate::util::text_region::cells_to_text_regions(
            cells,
            w,
            h,
            content_start_y,
            x_off,
            text_max_w,
        )
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
    /// Approximate live bytes of one cached entry (rendered cells + region
    /// strings). Only the byte-heavy `Vec<Cell>` and the region text are
    /// counted — the fixed per-message overhead is negligible. An associated
    /// function (no `&self`) so the store sites can compute it from disjoint
    /// field borrows while the reusable `scratch` buffer is still borrowed.
    fn cache_entry_bytes_of(
        cells: &Option<Vec<ratatui::buffer::Cell>>,
        regions: &Option<Vec<TextRegion>>,
    ) -> usize {
        let cells = cells.as_ref().map_or(0, |c| {
            c.len()
                .saturating_mul(std::mem::size_of::<ratatui::buffer::Cell>())
        });
        let regions = regions.as_ref().map_or(0, |r| {
            r.iter().fold(0usize, |acc, reg| {
                acc.saturating_add(reg.text.capacity())
                    .saturating_add(std::mem::size_of::<TextRegion>())
            })
        });
        cells.saturating_add(regions)
    }

    /// Evict per-message render-cache entries (cells + text regions) LRU-style
    /// until the cache is back under [`RENDER_CACHE_LOW_WATER`]. Messages
    /// inside a generous row guard around the viewport are never evicted, so
    /// ordinary scrolling stays re-render-free; only old content far from the
    /// current position sheds its cached cells. The transcript is untouched —
    /// an evicted message simply re-renders if the user scrolls back to it.
    ///
    /// The guard window is located with two binary searches over `prefix_y`
    /// (O(log n)) instead of a full timeline walk with a running row counter:
    /// [`find_first_visible`](Self::find_first_visible) bounds the messages
    /// entirely above the guard and a mirror search bounds the messages
    /// entirely below it. The candidate scan is then limited to the two
    /// evictable ranges with a single cached-cell check per message (no
    /// position arithmetic), but it still scales with the number of messages
    /// OUTSIDE the guard — so when the viewport sits at the bottom of a long
    /// timeline the front range covers most of it. The win is the O(log n)
    /// boundaries, the O(1) per-item work, and skipping the guard interior.
    /// The eviction then sizes and orders only the entries it will evict: a
    /// partial selection finds the smallest recency-order window that covers
    /// the overshoot and sorts that window alone, instead of sorting every
    /// candidate.
    fn prune_render_cache(&mut self, vp_top: i32, vp_bottom: i32) {
        if self.msg_cache_tokens.is_empty() {
            return;
        }
        let n = self.msg_cache_tokens.len();
        // The render path resizes every render-cache vector and `prefix_y`
        // together, but guard cheaply against direct use elsewhere. The
        // assert surfaces invariant drift in debug builds instead of the
        // early return masking it silently.
        debug_assert_eq!(self.msg_height_cache.len(), n);
        debug_assert_eq!(self.prefix_y.len(), n + 1);
        if self.msg_height_cache.len() != n || self.prefix_y.len() != n + 1 {
            return;
        }
        // Content-space guard: the visible content window is
        // [scroll_y, scroll_y + viewport_height], and the message walk below
        // is in content rows. `vp_top`/`vp_bottom` are SCREEN rows — only
        // their difference (the viewport height) is coordinate-independent.
        let visible_h = vp_bottom - vp_top;
        let guard_top = self.scroll_y - RENDER_CACHE_GUARD_ROWS;
        let guard_bottom = self.scroll_y + visible_h + RENDER_CACHE_GUARD_ROWS;

        // Guard window via binary search over `prefix_y` (both O(log n)):
        // `above_guard_end` is the first message whose bottom crosses
        // `guard_top` (messages [0, above_guard_end) sit entirely above the
        // guard); `below_guard_start` is the first message whose top is at or
        // after `guard_bottom` (messages [below_guard_start, n) sit entirely
        // below). Messages in [above_guard_end, below_guard_start) intersect
        // the guard and are never evicted. `prefix_y` is monotonic (heights
        // are non-negative), so the two boundaries never cross.
        let above_guard_end = self.find_first_visible(guard_top);
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.prefix_y[mid] >= guard_bottom {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let below_guard_start = lo;

        // Only the two evictable ranges are scanned, with the guard test
        // already baked into the boundaries — per-message work is a single
        // cached-cell check (no position arithmetic).
        let mut candidates: Vec<usize> =
            Vec::with_capacity(above_guard_end + (n - below_guard_start));
        for idx in 0..above_guard_end {
            if self.msg_cache_cells[idx].is_some() {
                candidates.push(idx);
            }
        }
        for idx in below_guard_start..n {
            if self.msg_cache_cells[idx].is_some() {
                candidates.push(idx);
            }
        }
        if candidates.is_empty() {
            return;
        }
        let overshoot = self.msg_cache_bytes.saturating_sub(RENDER_CACHE_LOW_WATER);
        if overshoot == 0 {
            return;
        }
        // Evict oldest-first until back under the low-water mark. Only a
        // prefix of the recency order is ever evicted (the byte crossing
        // lands inside the first `m` candidates), so instead of sorting the
        // whole candidate list (O(k log k)) find the smallest power-of-two
        // window whose cumulative size covers the overshoot: each
        // `select_nth_unstable_by_key` partitions so the `window` oldest
        // candidates are up front (O(k) each), and the window doubles
        // between probes. Only that window is then sorted and walked.
        // With distinct stamps the result matches a stable full sort
        // exactly; equal stamps (messages rendered in the same frame share
        // the render-frame stamp) are broken arbitrarily by the partition,
        // which is fine for an LRU — the guard, the outside-guard-only
        // eviction and the crossing semantics are preserved either way.
        // The window degrades to the full list only when nearly every
        // candidate is evicted (covered == k).
        let mut window = 1usize;
        let covered = loop {
            if window >= candidates.len() {
                break candidates.len();
            }
            candidates.select_nth_unstable_by_key(window, |&i| self.msg_cache_last_used[i]);
            let sum: usize = candidates[..window]
                .iter()
                .map(|&i| {
                    Self::cache_entry_bytes_of(
                        &self.msg_cache_cells[i],
                        &self.msg_cache_text_regions[i],
                    )
                })
                .sum();
            if sum >= overshoot {
                break window;
            }
            window *= 2;
        };
        candidates[..covered].sort_by_key(|&i| self.msg_cache_last_used[i]);
        for &idx in &candidates[..covered] {
            if self.msg_cache_bytes <= RENDER_CACHE_LOW_WATER {
                break;
            }
            self.msg_cache_bytes = self
                .msg_cache_bytes
                .saturating_sub(Self::cache_entry_bytes_of(
                    &self.msg_cache_cells[idx],
                    &self.msg_cache_text_regions[idx],
                ));
            self.msg_cache_tokens[idx] = !0;
            self.msg_cache_w[idx] = 0;
            self.msg_cache_h[idx] = 0;
            self.msg_cache_cells[idx] = None;
            self.msg_cache_text_regions[idx] = None;
        }
    }

    /// Index of the first message that can intersect the viewport, found with
    /// a binary search over `prefix_y` instead of walking the whole timeline:
    /// the smallest `i` whose content bottom (`prefix_y[i] + msg_height_cache[i]`)
    /// is strictly below the viewport top (`scroll_y` in content space). The
    /// per-frame message walk then starts here and stops once `msg_top` passes
    /// `vp_bottom`, so the frame cost is O(log n + visible) instead of O(n).
    ///
    /// Messages whose estimated height is 0 occupy no content rows and can
    /// never satisfy the render overlap test (`msg_bottom > vp_top &&
    /// msg_top < vp_bottom` with `msg_bottom == msg_top`), so skipping them is
    /// safe — the walk would have skipped them anyway.
    fn find_first_visible(&self, scroll_y: i32) -> usize {
        let n = self.msg_height_cache.len();
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let bottom = self.prefix_y[mid].saturating_add(self.msg_height_cache[mid]);
            if bottom > scroll_y {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
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
        tool_state: &mut ToolRenderState,
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
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(part, max_w, config, role, tool_state)
                        });
                    let render_h = if streaming {
                        // During streaming, use the exact estimated height without padding.
                        // estimate_height uses the same pulldown_cmark layout algorithm as
                        // the render, so it's already accurate. Adding padding would inflate
                        // the total height and create scroll oscillation frame-to-frame.
                        est_h.max(1).min(bottom.saturating_sub(y))
                    } else {
                        // Small +1 min(3) padding for non-streaming messages to provide a
                        // safety margin against estimate_height being slightly off (CJK widths,
                        // code block spacing, etc.). This prevents content clipping without
                        // causing the oscillation that the old +5/min(10) padding caused.
                        // scan_content_height will return the actual content height, and
                        // without .max() inflation at end of render, no oscillation occurs
                        // even if actual_h slightly exceeds est_h.
                        (est_h.saturating_add(1))
                            .max(3)
                            .min(bottom.saturating_sub(y))
                    };
                    let area = Rect::new(x, y, max_w, render_h);
                    let mut md = cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                        Some(content),
                    );
                    md.set_fg(Some(ColorInput::RGBA(fg_color)));
                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                    crate::util::markdown::apply_theme(&mut md, theme);
                    md.render_self(buf, area);
                    // During streaming, skip the expensive scan_content_height
                    // for the last message since it will be re-rendered next
                    // frame. The estimated height from the same markdown layout
                    // algorithm is already accurate, so no padding is needed.
                    let actual_h = if streaming {
                        render_h
                    } else {
                        // The glyph scan ignores the code block's padding rows
                        // (top gap, bottom padding and blank separator), so
                        // advancing purely by the scan would make the next
                        // part/message overlap them — which visually erases the
                        // block's bottom padding (code blocks without a
                        // language tag ended up with 2 blank rows on top and
                        // none at the bottom). The estimate mirrors the
                        // renderer's full cursor advance, so advance by
                        // whichever is taller.
                        let scanned = Self::scan_content_height(buf, x, y, max_w, render_h);
                        scanned.max(est_h)
                    };
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
                        .unwrap_or_else(|| {
                            Self::estimate_part_height(part, max_w, config, role, tool_state)
                        })
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
                    // so we can add a small vertical margin around it. Uses the
                    // same predicate as the height estimate (`tool_is_block`),
                    // so the drawn box plus margins matches the cached height.
                    let is_block = Self::tool_is_block(tool);

                    // Top margin — always added for block tools (the estimate
                    // reserves it even for the first part of the message), but
                    // skip if there isn't room for at least 1 row after it.
                    if is_block && y + 1 < bottom {
                        y += 1;
                    }

                    let mut line_h = 0u16;
                    tool_render::dispatch_tool(
                        &mut tool_render::ToolRenderCtx {
                            buf: &mut *buf,
                            x,
                            y,
                            line_h: &mut line_h,
                            max_w,
                            state: &mut *tool_state,
                            theme,
                        },
                        tool,
                        pi as u16,
                    );
                    let available = bottom.saturating_sub(y);
                    line_h = line_h.min(available);
                    y += line_h;

                    // Bottom margin
                    if is_block && y < bottom {
                        y += 1;
                    }

                    // Passive LSP findings: a "+"/"- LSP" affordance in the
                    // Thought style, collapsed by default — a click expands
                    // one clipped line per finding below the code/diff block.
                    if let Some(notes) = &tool.lsp_notes
                        && matches!(tool.status, ToolStatus::Completed)
                        && Self::lsp_notes_count(notes) > 0
                    {
                        let expanded = tool_state.is_expanded_or(
                            &Self::lsp_notes_id(tool),
                            config.diagnostics_mode,
                        );
                        let header = if expanded { "- Diagnostics" } else { "+ Diagnostics" };
                        let header_style = Style::default().fg(rgba_color(theme.error));
                        draw_text_line(buf, header, x, y, max_w, header_style);
                        y += 1;
                        if expanded {
                            y = Self::render_lsp_notes(buf, x, y, max_w, theme, notes);
                        }
                    }
                }
                Part::Reasoning(r) => {
                    let expanded = tool_state.is_expanded_or(
                        &r.text[..r.text.floor_char_boundary(32)],
                        config.thinking_mode,
                    );
                    let mut line_h = 0u16;
                    Self::render_reasoning(buf, x, y, &mut line_h, max_w, r, expanded, theme);
                    y += line_h.max(1);
                }
                Part::File(f) => {
                    Self::render_file_badge(buf, x, y, max_w, theme, f);
                    y += 1;
                }
                Part::Compaction(c) => {
                    if !c.text.is_empty() {
                        let expanded = tool_state.is_expanded(&summarizing_id(c));
                        let mut line_h =
                            Self::render_summarizing_box(buf, x, y, max_w, c, expanded, theme);
                        // Clamp to the remaining viewport like the tool blocks,
                        // so an expanded long summary never draws past it.
                        let available = bottom.saturating_sub(y);
                        line_h = line_h.min(available);
                        y += line_h;
                    } else {
                        // One-line status: the "Summarizing" line ticks live
                        // (computed from `started_at` vs now) until it
                        // finalizes. Height is always 1, so the line never
                        // reflows while the stopwatch runs.
                        let text = compaction_line(c, crate::types::now_ms());
                        let style = Style::default().fg(rgba_color(compaction_color(theme)));
                        draw_text_line(buf, &text, x, y, max_w, style);
                        y += 1;
                    }
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

    /// True when the tool renders as a bordered block (box) rather than an
    /// inline single-line label. Mirrors what the renderers actually draw so
    /// the height estimate and the render agree on the box's external margins:
    /// `render_shell` boxes any bash with output (Running or Completed),
    /// `render_glob` boxes any glob with listable output, `render_write` /
    /// `render_edit` box only completed tools, `render_read` boxes any
    /// completed read with parseable content, `render_todo` boxes any
    /// non-running tool with listable output (Completed and Failed both draw).
    fn tool_is_block(part: &ToolPart) -> bool {
        let display = tool_render::tool_display(&part.tool);
        let has_output = part.output.as_deref().is_some_and(|o| !o.trim().is_empty());
        match display {
            "bash" => has_output,
            "glob" => tool_render::glob_block_text(part).is_some(),
            "write" | "edit" => has_output && matches!(part.status, ToolStatus::Completed),
            "read" => {
                has_output
                    && matches!(part.status, ToolStatus::Completed)
                    && tool_render::read_block_text(part).is_some()
            }
            "todo" => {
                !matches!(part.status, ToolStatus::Running)
                    && has_output
                    && !tool_render::format_todo_output(
                        part.output.as_deref().unwrap_or("").trim(),
                        &part.tool,
                    )
                    .is_empty()
            }
            _ => false,
        }
    }

    fn estimate_part_height(
        part: &Part,
        max_w: u16,
        config: &TuiConfig,
        role: &MessageRole,
        tool_state: &ToolRenderState,
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
                // Completed ask_questions renders its Q&A summary as plain
                // markdown on the chat background (like an assistant text
                // part) — use the same markdown height estimator.
                if tool_render::tool_display(&t.tool) == "question"
                    && let Some(body) = tool_render::question_markdown(t)
                {
                    return estimate_height(&body, max_w).max(1);
                }
                // Only tools that render block-style output (shell, write, edit, read, todo, glob)
                // should allocate height for the full output block. All other tool
                // types render inline (1 line) regardless of whether they have output.
                // The conditions mirror each renderer EXACTLY, so the cached height
                // matches the drawn box even mid-loop (Running bash/glob with
                // streamed output already draw their box).
                let is_block = Self::tool_is_block(t);
                let notes_h =
                    Self::lsp_notes_height(t, tool_state, config.diagnostics_mode);
                if is_block {
                    let output = t.output.as_deref().unwrap_or("").trim();
                    // Add 2 rows for the block's internal padding (top/bottom border lines),
                    // plus 2 rows for the external vertical margin that render_parts adds
                    // around block-type tools (1 top, 1 bottom).
                    if tool_render::tool_display(&t.tool) == "glob" {
                        // Glob block: preview collapsed, full list when expanded
                        // (mirrors render_glob's collapse_tool_output).
                        let id = t.tool_call_id.as_deref().unwrap_or("glob");
                        let body = self::tool_render::glob_block_text(t).unwrap_or_default();
                        let collapsed = crate::util::scroll::collapse_tool_output(&body, 10, 800);
                        let expanded = tool_state.is_expanded(id);
                        let display = if expanded || !collapsed.overflow {
                            &body
                        } else {
                            &collapsed.output
                        };
                        let lines =
                            display.lines().count().max(1) as u16 + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        lines + 5
                    } else if tool_render::tool_display(&t.tool) == "todo" {
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
                    } else if tool_render::tool_display(&t.tool) == "read" {
                        // Read block: preview collapsed to a few lines, full
                        // code when expanded (mirrors render_read).
                        let id = t.tool_call_id.as_deref().unwrap_or("read");
                        let code = tool_render::read_block_text(t).unwrap_or_default();
                        let collapsed = crate::util::scroll::collapse_tool_output(&code, 10, 800);
                        let expanded = tool_state.is_expanded(id);
                        let display = if expanded || !collapsed.overflow {
                            &code
                        } else {
                            &collapsed.output
                        };
                        let lines =
                            display.lines().count().max(1) as u16 + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        lines + 5
                    } else {
                        // bash (and write) block. When the tool is expanded, the full
                        // output is rendered, so the height must match render_shell.
                        let id = t.tool_call_id.as_deref().unwrap_or("shell");
                        let collapsed = crate::util::scroll::collapse_tool_output(output, 10, 800);
                        let display = if tool_state.is_expanded(id) {
                            output
                        } else {
                            &collapsed.output
                        };
                        let lines =
                            display.lines().count().max(1) as u16 + u16::from(collapsed.overflow);
                        // Internal box: top padding (1) + title + content +
                        // bottom padding (1) = lines + 3; +2 external margins.
                        lines + 5
                    }
                } else {
                    1
                }
                .saturating_add(notes_h)
            }
            Part::Reasoning(r) => {
                if r.text.is_empty() {
                    1
                } else {
                    let expanded = tool_state.is_expanded_or(
                        &r.text[..r.text.floor_char_boundary(32)],
                        config.thinking_mode,
                    );
                    if expanded {
                        // Header row + the wrap-aware markdown body height —
                        // the same estimate render_reasoning lays out with,
                        // so the cached height matches the drawn block even
                        // when long paragraphs wrap into extra rows.
                        let body_w = max_w.saturating_sub(2).max(1);
                        1u16.saturating_add(estimate_height(&sanitize_text(&r.text), body_w).max(1))
                    } else {
                        // Collapsed: just the "+ Thought" header row.
                        1
                    }
                }
            }
            Part::File(_) => 1,
            Part::Compaction(c) => {
                if !c.text.is_empty() {
                    // The "Summarizing" box: title row + wrapped body. Collapsed
                    // keeps a fixed preview height (scroll-up); expanded grows
                    // with the full streamed text. The width is the box's inner
                    // width, so wrapped rows match the render exactly.
                    let expanded = tool_state.is_expanded(&summarizing_id(c));
                    summarizing_height(c, expanded, max_w)
                } else {
                    1
                }
            }
            Part::Text(_) => 0,
        }
    }

    /// Rows the passive LSP findings occupy for a completed tool: the
    /// `+`/`- LSP` header, plus one clipped line per finding when expanded.
    /// Must stay in sync with the notes drawn at the end of the
    /// `Part::Tool` branch in `render_parts`.
    fn lsp_notes_height(
        t: &ToolPart,
        tool_state: &ToolRenderState,
        default_expanded: bool,
    ) -> u16 {
        match (&t.lsp_notes, &t.status) {
            (Some(n), ToolStatus::Completed) => {
                let count = Self::lsp_notes_count(n);
                if count == 0 {
                    0
                } else if tool_state
                    .is_expanded_or(&Self::lsp_notes_id(t), default_expanded)
                {
                    1 + count
                } else {
                    1
                }
            }
            _ => 0,
        }
    }

    /// Stable expansion key for a tool part's LSP block.
    fn lsp_notes_id(t: &ToolPart) -> String {
        format!("lsp:{}", t.tool_call_id.as_deref().unwrap_or(&t.tool))
    }

    /// Total findings across severities.
    fn lsp_notes_count(n: &cosh_tools::fs::LspNotes) -> u16 {
        (n.errors.len() + n.warnings.len()).min(u16::MAX as usize) as u16
    }

    /// Draw the passive LSP findings below a tool's output block: errors in
    /// the theme's error color, warnings in its warning color.
    fn render_lsp_notes(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        max_w: u16,
        theme: &Theme,
        notes: &cosh_tools::fs::LspNotes,
    ) -> u16 {
        let error_style = Style::default().fg(rgba_color(theme.error));
        let warning_style = Style::default().fg(rgba_color(theme.warning));
        let mut y = y;
        for note in &notes.errors {
            let line = format!("\u{2717} {}:{} {}", note.path, note.line, note.message);
            draw_text_line(buf, &line, x, y, max_w, error_style);
            y += 1;
        }
        for note in &notes.warnings {
            let line = format!("\u{26a0} {}:{} {}", note.path, note.line, note.message);
            draw_text_line(buf, &line, x, y, max_w, warning_style);
            y += 1;
        }
        y
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

    #[allow(clippy::too_many_arguments)]
    fn render_user_message(
        buf: &mut Buffer,
        area: Rect,
        msg: &Message,
        theme: &Theme,
        agent_color: RGBA,
        tool_state: &mut ToolRenderState,
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
        tool_state: &mut ToolRenderState,
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
            for (line_y, line) in (area.y + 1..).zip(lines.iter()) {
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

        banner_h + parts_h
    }

    /// Locate the message index under content row `click_y` using the
    /// prefix-y binary search. Shared by the click handler and the hover
    /// tracker.
    fn message_index_at_y(&self, vp_top: i32, click_y: i32, len: usize) -> Option<usize> {
        let mut low = 0;
        let mut high = len;
        while low < high {
            let mid = low + (high - low) / 2;
            let msg_top = vp_top - self.scroll_y + self.prefix_y[mid];
            let msg_bottom = msg_top + self.msg_height_cache[mid];
            if click_y < msg_top {
                high = mid;
            } else if click_y >= msg_bottom {
                low = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    /// Track the hovered user message on mouse-move events (opencode-style).
    /// Returns `true` when the hover target changed so the caller redraws.
    pub fn update_hover(
        &mut self,
        y: u16,
        area: Rect,
        state: &AppState,
        config: &TuiConfig,
    ) -> bool {
        let Some(session) = state.current_session() else {
            return self.clear_hover();
        };
        // Hover stays active while the agent works: the highlight is purely
        // visual and Copy remains available during streaming.
        if self.drag_selection.is_some() {
            return self.clear_hover();
        }

        let margin = 2;
        let inner_area = Rect::new(
            area.x + margin,
            area.y,
            area.width.saturating_sub(margin * 2),
            area.height,
        );
        let max_w = inner_area.width.saturating_sub(6).max(2);
        self.ensure_height_caches_fresh(session, max_w, config, None);

        let next = self
            .message_index_at_y(
                i32::from(inner_area.y),
                i32::from(y),
                session.messages.len(),
            )
            .filter(|&idx| session.messages[idx].role == MessageRole::User);
        if next != self.hovered_msg_idx {
            self.hovered_msg_idx = next;
            return true;
        }
        false
    }

    /// Clear the hovered user-message highlight (the opencode-style row tint).
    /// `true` when the hover target was set and is now cleared.
    pub fn clear_hover(&mut self) -> bool {
        if self.hovered_msg_idx.is_some() {
            self.hovered_msg_idx = None;
            return true;
        }
        false
    }

    /// Recolor the hovered user message's cells with the element background —
    /// the same visual swap opencode applies via
    /// `backgroundColor={hover() ? theme.backgroundElement : ...}`.
    fn render_hover_highlight(
        &self,
        buf: &mut Buffer,
        inner_area: Rect,
        session: &crate::types::Session,
        theme: &Theme,
    ) {
        let Some(idx) = self.hovered_msg_idx else {
            return;
        };
        if idx >= self.prefix_y.len().saturating_sub(1) || idx >= self.msg_height_cache.len() {
            return;
        }
        if session
            .messages
            .get(idx)
            .is_none_or(|m| m.role != MessageRole::User)
        {
            return;
        }
        let vp_top = i32::from(inner_area.y);
        let vp_bottom_excl = i32::from(inner_area.y + inner_area.height);
        let msg_top = (vp_top - self.scroll_y + self.prefix_y[idx]).max(vp_top);
        let msg_bottom = (vp_top - self.scroll_y + self.prefix_y[idx] + self.msg_height_cache[idx])
            .min(vp_bottom_excl);
        for cy in msg_top..msg_bottom {
            for cx in inner_area.x..inner_area.x + inner_area.width {
                if let Some(cell) = buf.cell_mut((cx, u16::try_from(cy).unwrap_or(0))) {
                    cell.set_bg(rgba_color(theme.background_element));
                }
            }
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
        let max_w = inner_area.width.saturating_sub(6).max(2);
        let x_off = i32::from(inner_area.x + 3);

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());
        let click_x = i32::from(mouse.x);
        let click_y = i32::from(mouse.y);

        self.ensure_height_caches_fresh(session, max_w, config, None);

        // Binary search for the message that contains the click position,
        // using the prefix-y array directly (no O(n) per-click allocation of
        // a position vector). Message `i` starts at
        // `vp_top - scroll_y + prefix_y[i]` in screen space — the same layout
        // the render walk produces.
        let mut low = 0;
        let mut high = session.messages.len();
        let mut target_msg_idx = None;
        while low < high {
            let mid = low + (high - low) / 2;
            let msg_top = vp_top - self.scroll_y + self.prefix_y[mid];
            let msg_h = self.msg_height_cache[mid];
            let msg_bottom = msg_top + msg_h;

            if click_y < msg_top {
                high = mid;
            } else if click_y >= msg_bottom {
                low = mid + 1;
            } else {
                target_msg_idx = Some(mid);
                break;
            }
        }

        // 3. Confirm if message is visible in viewport
        if let Some(idx) = target_msg_idx {
            let msg = &session.messages[idx];
            let msg_h = self.msg_height_cache[idx];
            let msg_top = vp_top - self.scroll_y + self.prefix_y[idx];
            let msg_bottom = msg_top + msg_h;

            // Check if click is within this message and message is visible
            if click_y >= msg_top
                && click_y < msg_bottom
                && msg_bottom > vp_top
                && msg_top < vp_bottom
            {
                // Click on a user message → request the Message Actions
                // dialog (app.rs opens it). Assistant messages keep the
                // per-part click handling below.
                if msg.role == MessageRole::User {
                    self.pending_message_action = Some(msg.id.clone());
                    return true;
                }
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
                        // Passive LSP findings: the last rows of the part
                        // belong to the `+`/`- LSP` block — any click there
                        // toggles it, ahead of the tool block's own toggle.
                        // Mirrors the renderer's visibility gates so hidden
                        // parts never claim clicks for absent rows.
                        if let crate::types::Part::Tool(tool) = part
                            && tool.lsp_notes.is_some()
                            && matches!(tool.status, ToolStatus::Completed)
                            && config.show_tool_details
                            && (config.show_generic_tool_output
                                || tool_render::tool_display(&tool.tool) != "generic")
                        {
                            let notes_rows =
                                i32::from(Self::lsp_notes_height(
                                    tool,
                                    &self.tool_state,
                                    config.diagnostics_mode,
                                ));
                            if notes_rows > 0
                                && click_y >= part_y + part_h - notes_rows
                            {
                                let id = Self::lsp_notes_id(tool);
                                self.tool_state.toggle_with_default(
                                    &id,
                                    config.diagnostics_mode,
                                );
                                return true;
                            }
                        }

                        if let crate::types::Part::Tool(tool) = part {
                            let display = tool_render::tool_display(&tool.tool);
                            if display == "bash" || display == "glob" || display == "read" {
                                let output =
                                    tool.output.as_deref().unwrap_or("").trim().to_string();
                                if !output.is_empty() {
                                    let id =
                                        tool.tool_call_id.as_deref().unwrap_or(match display {
                                            "glob" => "glob",
                                            "read" => "read",
                                            _ => "shell",
                                        });
                                    let text = if display == "glob" {
                                        tool_render::glob_block_text(tool).unwrap_or_default()
                                    } else if display == "read" {
                                        tool_render::read_block_text(tool).unwrap_or_default()
                                    } else {
                                        output
                                    };
                                    let collapsed =
                                        crate::util::scroll::collapse_tool_output(&text, 10, 800);
                                    if collapsed.overflow {
                                        // Toggle on any click inside the block. The part's
                                        // row range already matches the rendered block because the
                                        // height caches are rebuilt with the expansion state.
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
                                    // Flip relative to the effective state so
                                    // the first click on a block expanded via
                                    // the global thinking mode collapses it.
                                    self.tool_state
                                        .toggle_with_default(part_id, config.thinking_mode);
                                    return true;
                                }
                            }
                        }

                        if let crate::types::Part::Compaction(c) = part
                            && !c.text.is_empty()
                        {
                            // Toggle the "Summarizing" box on any click
                            // inside it (the part's row range matches the
                            // rendered box — height caches are rebuilt with
                            // the expansion state).
                            let id = summarizing_id(c);
                            self.tool_state.toggle_expanded(&id);
                            return true;
                        }

                        return true;
                    }

                    part_y += part_h;
                }
            }
        }

        false
    }

    pub fn render_message_height(
        msg: &Message,
        max_w: u16,
        config: &TuiConfig,
        part_heights: Option<&[u16]>,
        tool_state: &ToolRenderState,
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
                .map(|p| {
                    i32::from(Self::estimate_part_height(
                        p, max_w, config, &msg.role, tool_state,
                    ))
                })
                .sum()
        };
        let padding_bottom: i32 = match msg.role {
            MessageRole::User => 1,
            MessageRole::Assistant if is_error => 2,
            MessageRole::Assistant => 0,
        };
        border_h + parts_h + padding_bottom
    }

    /// Rebuild `prefix_y` from the height cache and return the total content
    /// height. The walk places message `i` at
    /// `sum(msg_height_cache[..i]) + i` (each message after the first is
    /// preceded by a 1-row gap, exactly like the render walk's `idx > 0`
    /// logic), so `prefix_y[i]` = that position and `prefix_y[n]` = the total
    /// (last message's height, no trailing gap).
    fn rebuild_prefix_y(&mut self) -> i32 {
        self.prefix_y.clear();
        let n = self.msg_height_cache.len();
        if n == 0 {
            self.prefix_y.push(0);
            return 0;
        }
        let mut y: i32 = 0;
        self.prefix_y.push(0); // position of message 0
        for idx in 1..n {
            y += 1 + self.msg_height_cache[idx - 1];
            self.prefix_y.push(y);
        }
        y += self.msg_height_cache[n - 1]; // final height, no trailing gap
        self.prefix_y.push(y);
        y
    }

    /// Maintain the persistent render cache for the streaming last message.
    ///
    /// While tokens append to the last `Text` part, this renders the message's
    /// stable non-text parts once and patches only the streaming text part's
    /// tail each frame (O(delta)), so the walk can blit the whole message from
    /// `self.streaming_msg.cells` instead of re-parsing and re-rendering the
    /// growing message every frame. Returns the per-part heights for the
    /// height cache, or `None` when nothing is streaming (the caller falls
    /// back to the regular per-message cache path).
    fn update_streaming_message_cache(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        streaming: bool,
    ) -> Option<streaming::StreamState> {
        if !streaming {
            self.streaming_msg = None;
            return None;
        }
        let Some(msg) = session.messages.last() else {
            self.streaming_msg = None;
            return None;
        };
        // Only a non-error assistant message whose LAST part is a growing Text
        // part can be rendered incrementally. Anything else (a running tool
        // box, an error message, ...) falls back to the regular full-render
        // path.
        if msg.role != MessageRole::Assistant
            || msg.id.starts_with("msg-err-")
            || !matches!(msg.parts.last(), Some(Part::Text(t)) if !t.synthetic)
        {
            self.streaming_msg = None;
            return None;
        }
        let config_tok = config_token(config);
        let width = inner_area.width;
        let text_pi = msg.parts.len() - 1;

        let identity_ok = self.streaming_msg.as_ref().is_some_and(|sc| {
            sc.message_id == msg.id
                && sc.width == width
                && sc.max_w == max_w
                && sc.config_token == config_tok
                && sc.tool_state_version == self.tool_state.version
                && sc.part_hs.len() == msg.parts.len()
                && sc.text.as_ref().is_some_and(|t| t.part_index == text_pi)
        });

        let Part::Text(text_part) = &msg.parts[text_pi] else {
            unreachable!();
        };

        if !identity_ok {
            // ── Fresh cache: render the whole message once ──
            let mut part_hs: Vec<u16> = msg
                .parts
                .iter()
                .enumerate()
                .map(|(pi, p)| {
                    if pi == text_pi {
                        1 // placeholder; patched below from the text cache
                    } else {
                        Self::estimate_part_height(p, max_w, config, &msg.role, &self.tool_state)
                    }
                })
                .collect();
            let mut text_cache = streaming::StreamingTextCache::new(
                msg.id.clone(),
                text_pi,
                max_w,
                config_tok,
                theme,
            );
            let (_, text_h) = text_cache.update(&text_part.text, max_w, config_tok, config.conceal);
            part_hs[text_pi] = text_h;
            let msg_h: u16 = part_hs.iter().sum::<u16>().max(1);
            let full_area = Rect::new(0, 0, width, msg_h);
            let temp = self.scratch.get_or_insert_with(|| Buffer::empty(full_area));
            if *temp.area() != full_area {
                temp.resize(full_area);
            }
            temp.reset();
            temp.set_style(full_area, Style::default().bg(rgba_color(theme.background)));
            Self::render_assistant_message(
                temp,
                full_area,
                msg,
                theme,
                &mut self.tool_state,
                config,
                false,
                false,
                &part_hs,
                true,
            );
            let part_offset: u16 = part_hs[..text_pi].iter().sum();
            let mut cells = Vec::with_capacity(width as usize * msg_h as usize);
            let temp_cells = temp.content();
            let stride = width as usize;
            for dy in 0..msg_h as usize {
                for dx in 0..stride {
                    cells.push(temp_cells[dy * stride + dx].clone());
                }
            }
            self.streaming_msg = Some(streaming::StreamingMessageCache {
                message_id: msg.id.clone(),
                width,
                max_w,
                config_token: config_tok,
                tool_state_version: self.tool_state.version,
                part_hs,
                cells,
                height: msg_h,
                text: Some(text_cache),
                last_tail: Some((part_offset, part_offset + text_h)),
            });
        } else {
            // ── Incremental: patch the streaming text part's tail ──
            let sc = self.streaming_msg.as_mut().unwrap();
            let text_cache = sc.text.as_mut().unwrap();
            let (render_row, text_h) =
                text_cache.update(&text_part.text, max_w, config_tok, config.conceal);
            sc.part_hs[text_pi] = text_h;
            let msg_h: u16 = sc.part_hs.iter().sum::<u16>().max(1);
            let old_h = sc.height;
            let x_off = 3u16; // text column within the message cells (area-relative)
            let part_offset: u16 = sc.part_hs[..text_pi].iter().sum();
            let row_end = part_offset + text_h;

            // Grow the cell buffer, preserving content; fill the new rows with
            // the background and carry the border glyph down.
            let stride = width as usize;
            let new_cap = stride * msg_h as usize;
            if new_cap > sc.cells.len() {
                let border = sc.cells.first().cloned().unwrap_or_default();
                sc.cells.resize(new_cap, Cell::default());
                for dy in old_h..msg_h {
                    let base = dy as usize * stride;
                    for dx in 1..stride {
                        let mut c = Cell::default();
                        c.set_bg(rgba_color(theme.background));
                        c.set_char(' ');
                        sc.cells[base + dx] = c;
                    }
                    sc.cells[base] = border.clone();
                }
            }
            // Patch the re-rendered text rows from the text cache. The text
            // cache's buffer rows are indexed absolutely (the re-render was
            // written into `self.cells` at `area.y = render_row`), so text row
            // `dy` lives at `dy * tc_w` in the cache's content.
            let tc_cells = text_cache.content();
            let tc_w = max_w as usize;
            for dy in render_row..text_h {
                let dst_row = part_offset + dy;
                if dst_row >= msg_h {
                    break;
                }
                let dst_base = dst_row as usize * stride;
                let src_base = dy as usize * tc_w;
                for dx in 1..stride {
                    let mut c = Cell::default();
                    c.set_bg(rgba_color(theme.background));
                    c.set_char(' ');
                    sc.cells[dst_base + dx] = c;
                }
                for dx in 0..tc_w {
                    sc.cells[dst_base + x_off as usize + dx] = tc_cells[src_base + dx].clone();
                }
            }
            sc.height = msg_h;
            sc.last_tail = Some((part_offset + render_row, row_end));
        }

        let sc = self.streaming_msg.as_ref().unwrap();
        Some(streaming::StreamState {
            message_id: sc.message_id.clone(),
            part_hs: sc.part_hs.clone(),
            height: sc.height,
            raw_len: sc.text.as_ref().map_or(0, |t| t.raw_len()),
        })
    }

    /// Ensure height caches match the current session. Performs a full rebuild
    /// if terminal width changed or config changed. If only message count grew
    /// (new messages appended), extends the cache incrementally without touching
    /// existing entries. Otherwise, incrementally updates only the last message
    /// if its content changed (streaming — tool status, output, reasoning, text).
    /// Returns cache update details so callers can clear render caches and
    /// distinguish remote content growth from local expand/collapse layout.
    fn ensure_height_caches_fresh(
        &mut self,
        session: &crate::types::Session,
        max_w: u16,
        config: &TuiConfig,
        stream_state: Option<&streaming::StreamState>,
    ) -> HeightCacheUpdate {
        let config_tok = config_token(config);
        let session_changed = self.last_session_id.as_deref() != Some(session.id.as_str());
        let tool_state_changed = self.last_tool_state_version != self.tool_state.version;
        let cache_stale = session_changed
            || tool_state_changed
            || self.msg_height_cache.len() != session.messages.len()
            || self.cache_max_w != max_w
            || self.cache_config_token != config_tok;

        if cache_stale {
            let config_or_width_changed =
                self.cache_max_w != max_w || self.cache_config_token != config_tok;
            let count_grew =
                !config_or_width_changed && session.messages.len() > self.msg_height_cache.len();

            if config_or_width_changed || tool_state_changed || !count_grew || session_changed {
                // Full rebuild: config/width changed, or count decreased
                let _start = Instant::now();
                self.msg_height_cache.clear();
                self.part_heights_cache.clear();
                for m in session.messages.iter() {
                    let part_hs: Vec<u16> = m
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(p, max_w, config, &m.role, &self.tool_state)
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        m,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    self.part_heights_cache.push(part_hs);
                    self.msg_height_cache.push(msg_h);
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.msg_change_tokens = session.messages.iter().map(msg_change_token).collect();
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                self.prev_mutable_msgs = mutable_msg_indices(session);
                self.cached_total_height = self.rebuild_prefix_y();
                // When the session, expansion state, terminal width, or config
                // changed, the previous actual_total_height was computed for a
                // DIFFERENT layout and is NOT comparable to the new cached
                // height. Using .max() would preserve the old, larger value,
                // inflating max_scroll. This allows scroll_y to point beyond
                // the actual content, pushing all messages above the viewport.
                //
                // Only preserve the previous actual_total_height via .max()
                // when the layout is unchanged: during streaming, the rendered
                // height may slightly exceed the cache estimate, and .max()
                // prevents a scroll gap below the streaming message.
                if config_or_width_changed || tool_state_changed || session_changed {
                    self.actual_total_height = self.cached_total_height;
                } else {
                    self.actual_total_height =
                        self.actual_total_height.max(self.cached_total_height);
                }

                // When switching to a different session, scroll to the bottom
                // so the user sees the latest messages without manual scrolling.
                if session_changed {
                    self.scroll_y = self.cached_total_height;
                    self.has_manual_scroll = false;
                    self.is_sticky_bottom = true;
                    self.scroll_accumulator_y = 0.0;
                }

                log::debug!(
                    "[PERF] msg_height_cache: cold_build={}us msgs={}",
                    _start.elapsed().as_micros(),
                    session.messages.len()
                );
                HeightCacheUpdate {
                    full_rebuild: true,
                    tool_state_changed,
                }
            } else {
                // Extend cache with new messages only (same width/config)
                let prev_len = self.msg_height_cache.len();
                for m in session.messages.iter().skip(prev_len) {
                    let part_hs: Vec<u16> = m
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(p, max_w, config, &m.role, &self.tool_state)
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        m,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    self.part_heights_cache.push(part_hs);
                    self.msg_height_cache.push(msg_h);
                }
                self.cache_max_w = max_w;
                self.cache_config_token = config_tok;
                self.last_msg_change_token =
                    session.messages.last().map(msg_change_token).unwrap_or(0);
                self.msg_change_tokens
                    .extend(session.messages.iter().skip(prev_len).map(msg_change_token));
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                self.prev_mutable_msgs = mutable_msg_indices(session);
                // Incremental: add new messages' heights plus a gap for each.
                // All new messages have idx > 0 (prev_len >= 1), so gap = 1 per message.
                let new_count = self.msg_height_cache.len() - prev_len;
                let added_heights: i32 = self.msg_height_cache[prev_len..].iter().sum();
                self.cached_total_height += added_heights + new_count as i32;
                // Extend prefix_y: the previous last entry is the OLD total
                // (= the end of the last old message) — pop it, then each new
                // message starts one row below its predecessor's end (the
                // 1-row gap, except for the very first message of the
                // timeline) and ends after its own height. The final pushed
                // value is the new total.
                if self.prefix_y.is_empty() {
                    self.prefix_y.push(0);
                }
                let mut y = self.prefix_y.pop().unwrap_or(0);
                for (k, h) in self.msg_height_cache[prev_len..].iter().enumerate() {
                    if prev_len + k > 0 {
                        y += 1; // gap before this message
                    }
                    self.prefix_y.push(y); // its start position
                    y += h; // its end
                }
                self.prefix_y.push(y); // new total (no trailing gap)
                // Sync actual_total_height using .max() — see note above.
                self.actual_total_height = self.actual_total_height.max(self.cached_total_height);
                log::debug!(
                    "[PERF] msg_height_cache: extended prev={} now={}",
                    prev_len,
                    session.messages.len()
                );
                HeightCacheUpdate {
                    full_rebuild: false,
                    tool_state_changed,
                }
            }
        } else {
            // Incremental update: re-estimate ANY message that can still
            // change. Streaming text and tool completion normally touch only
            // the last message, but ToolResult/ToolError (app.rs) and glob/grep
            // ToolOutput (app.rs) search EVERY message for the running tool
            // part, so a box in an OLDER message can complete or grow while a
            // newer message streams below. Re-estimating it keeps the cached
            // height in sync with the box the render actually draws.
            //
            // Only messages that CAN change are re-hashed each pass: the last
            // message (always) plus any message with a Running tool / Running
            // compaction, plus those that had a running part LAST pass (a tool
            // completing in place between frames is no longer `Running`, but we
            // still catch it once so its final height lands). Everything else is
            // immutable, so hashing its full content per frame would pay
            // O(whole transcript) every frame for nothing.
            let mut any_changed = false;
            let mut changed_last = false;
            // When the streaming cache is live for the last message, its
            // per-part heights are authoritative and were already brought up to
            // date by `update_streaming_message_cache` this frame. Skip the
            // O(n) content hash + per-part re-estimate entirely.
            let stream_ident = stream_state.filter(|s| {
                session
                    .messages
                    .last()
                    .is_some_and(|m| s.message_id == m.id)
            });
            if !self.msg_height_cache.is_empty() {
                let last_idx = session.messages.len() - 1;
                // Take last pass's mutable set (carried over to catch an
                // in-place completion), then store this pass's for next time.
                let prev_mutable = std::mem::take(&mut self.prev_mutable_msgs);
                let curr_mutable = mutable_msg_indices(session);
                let is_candidate = |idx: usize| {
                    idx == last_idx
                        || prev_mutable.binary_search(&idx).is_ok()
                        || curr_mutable.binary_search(&idx).is_ok()
                };
                for (idx, msg) in session.messages.iter().enumerate() {
                    if !is_candidate(idx) {
                        continue;
                    }
                    if idx == last_idx
                        && let Some(s) = stream_ident
                    {
                        // Streaming message: use the cache's heights without
                        // hashing. The stored token is the cache height — a
                        // cheap monotonic stand-in; once streaming ends the
                        // normal `msg_change_token` path re-estimates once and
                        // replaces it.
                        if self.msg_height_cache.get(idx) != Some(&(s.height as i32)) {
                            if idx < self.part_heights_cache.len() {
                                self.part_heights_cache[idx] = s.part_hs.clone();
                                self.msg_height_cache[idx] = s.height as i32;
                            }
                            if idx < self.msg_change_tokens.len() {
                                self.msg_change_tokens[idx] = s.height as u64;
                            }
                            changed_last = true;
                            any_changed = true;
                        }
                        continue;
                    }
                    let token = msg_change_token(msg);
                    if token == self.msg_change_tokens.get(idx).copied().unwrap_or(u64::MAX) {
                        continue;
                    }
                    let part_hs: Vec<u16> = msg
                        .parts
                        .iter()
                        .map(|p| {
                            Self::estimate_part_height(
                                p,
                                max_w,
                                config,
                                &msg.role,
                                &self.tool_state,
                            )
                        })
                        .collect();
                    let msg_h = Self::render_message_height(
                        msg,
                        max_w,
                        config,
                        Some(&part_hs),
                        &self.tool_state,
                    );
                    if idx < self.part_heights_cache.len() {
                        self.part_heights_cache[idx] = part_hs;
                        self.msg_height_cache[idx] = msg_h;
                    }
                    if idx < self.msg_change_tokens.len() {
                        self.msg_change_tokens[idx] = token;
                    }
                    changed_last |= idx == last_idx;
                    any_changed = true;
                }
                self.prev_mutable_msgs = curr_mutable;
            }
            if any_changed {
                // The changed message's height moved, so every start position
                // after the first changed message shifts — rebuild the whole
                // prefix_y (O(n) sums) and the cached total.
                self.cached_total_height = self.rebuild_prefix_y();
                self.last_msg_change_token = if let Some(s) = stream_ident {
                    // Streaming: avoid the O(n) hash of the last message.
                    s.height as u64
                } else {
                    session.messages.last().map(msg_change_token).unwrap_or(0)
                };
                self.last_session_id = Some(session.id.clone());
                self.last_tool_state_version = self.tool_state.version;
                // Sync actual_total_height using .max() — see note above.
                self.actual_total_height = self.actual_total_height.max(self.cached_total_height);
                if self.render_frame.is_multiple_of(30) {
                    log::debug!(
                        "[PERF] msg_height_cache: incremental update (last={changed_last})"
                    );
                }
            }
            HeightCacheUpdate::default()
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
        self.build_text_regions_impl(session, inner_area, max_w, config, theme, None);
    }

    /// Like [`Self::build_text_regions`], but builds regions for an arbitrary
    /// content-space row span instead of the visible viewport. Used on
    /// drag-release so a selection that crossed scroll boundaries (auto-scroll)
    /// copies every row between anchor and focus, not just the last window.
    pub fn build_text_regions_for_content_range(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        content_range: (i32, i32),
    ) {
        self.build_text_regions_impl(
            session,
            inner_area,
            max_w,
            config,
            theme,
            Some(content_range),
        );
    }

    fn build_text_regions_impl(
        &mut self,
        session: &crate::types::Session,
        inner_area: Rect,
        max_w: u16,
        config: &TuiConfig,
        theme: &Theme,
        content_range: Option<(i32, i32)>,
    ) {
        // With a content range, run the same walk against a virtual viewport
        // of exactly that span: `scroll` shifts the walk to the range start
        // and vp_top=0 keeps every `screen - vp_top + scroll` expression
        // yielding absolute content coordinates.
        let (scroll, vp_top, vp_bottom) = if let Some((cs_start, cs_end)) = content_range {
            // Cap the span so a pathological selection can't allocate
            // unbounded region rows.
            const MAX_SELECTION_SPAN_ROWS: i32 = 100_000;
            let span = (cs_end - cs_start).clamp(1, MAX_SELECTION_SPAN_ROWS);
            (cs_start, 0, span)
        } else {
            (
                self.scroll_y,
                i32::from(inner_area.y),
                i32::from(inner_area.bottom()),
            )
        };
        let x_off = inner_area.x + 3;

        // Same O(log n + visible) walk as the render: start at the first
        // message that can intersect the viewport and stop past its bottom.
        // Off-screen messages contribute no regions, so skipping them is
        // identical to the old full walk — just without the per-frame O(n)
        // scan of the whole timeline. Computed BEFORE the field borrows
        // below (`find_first_visible` needs `&self`).
        let first_vis = self.find_first_visible(scroll);
        let walk_start = if self.msg_height_cache.is_empty() {
            vp_top - scroll
        } else {
            vp_top - scroll + self.prefix_y[first_vis]
        };

        // Direct field borrows keep the reusable scratch buffer (a `&mut`
        // borrow of `self.scratch`) disjoint from `self.text_regions` pushes
        // and the other fields touched inside the per-part loop.
        let text_regions = &mut self.text_regions;
        let scratch = &mut self.scratch;
        text_regions.clear();

        let mut y = walk_start;

        for (idx, msg) in session.messages.iter().enumerate().skip(first_vis) {
            if idx > first_vis {
                y += 1;
            }

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            if msg_top >= vp_bottom {
                break;
            }
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
                            Self::estimate_part_height(
                                part,
                                max_w,
                                config,
                                &msg.role,
                                &self.tool_state,
                            )
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
                                // ── Streaming last message: regions straight from the
                                // incremental cache ── `update_streaming_message_cache`
                                // (which runs before this in render()) already refreshed
                                // the text-part cells this frame, so only the viewport
                                // rows are converted to regions (O(visible)); no re-parse
                                // and no full-content sanitize per frame.
                                if msg.role == MessageRole::Assistant
                                    && self.streaming_msg.as_ref().is_some_and(|sc| {
                                        sc.message_id == msg.id
                                            && sc
                                                .text
                                                .as_ref()
                                                .is_some_and(|tc| tc.part_index == pi)
                                    })
                                {
                                    let sc = self.streaming_msg.as_ref().unwrap();
                                    if let Some(tc) = sc.text.as_ref() {
                                        let tc_cells = tc.content();
                                        let tc_w = tc.width as usize;
                                        let screen_start = p_top.max(vp_top) as u16;
                                        let screen_end = p_bottom.min(vp_bottom) as u16;
                                        if screen_start < screen_end {
                                            let first_dy = (screen_start as i32 - p_top) as u16;
                                            for (screen_line_y, dy) in
                                                (screen_start..).zip(first_dy..tc.height)
                                            {
                                                if screen_line_y >= screen_end {
                                                    break;
                                                }
                                                let base = dy as usize * tc_w;
                                                let mut line_text = String::with_capacity(tc_w);
                                                for dx in 0..tc_w {
                                                    line_text.push(
                                                        tc_cells[base + dx]
                                                            .symbol()
                                                            .chars()
                                                            .next()
                                                            .unwrap_or(' '),
                                                    );
                                                }
                                                let trimmed = line_text.trim_end().to_string();
                                                let cy = (screen_line_y as i32) - vp_top + scroll;
                                                text_regions.push(TextRegion {
                                                    y1: cy,
                                                    y2: cy + 1,
                                                    x1: x_off,
                                                    x2: x_off + max_w,
                                                    text: trimmed,
                                                });
                                            }
                                        }
                                    }
                                    part_y += part_h;
                                    continue;
                                }

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
                                                    text_regions.push(TextRegion {
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
                                        // Reuse the scratch buffer instead of allocating a
                                        // fresh multi-thousand-row buffer every frame while
                                        // the last message streams.
                                        let temp = scratch.get_or_insert_with(|| {
                                            ratatui::buffer::Buffer::empty(scan_area)
                                        });
                                        if *temp.area() != scan_area {
                                            temp.resize(scan_area);
                                        }

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
                                        crate::util::markdown::apply_theme(&mut md, theme);
                                        md.render_self(temp, scan_area);

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
                                            text_regions.push(TextRegion {
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
                                                text_regions.push(TextRegion {
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
                                            text_regions.push(TextRegion {
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
                                if self::tool_render::tool_display(&t.tool) == "todo"
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
                                    && self::tool_render::tool_display(&t.tool) == "generic"
                                {
                                    part_y += part_h;
                                    continue;
                                }
                                // Add inline tool label — but only when the
                                // renderer actually draws it. render_todo, for
                                // example, shows its label row only on failure
                                // (on success it renders just the list box),
                                // so copying it unconditionally leaked hidden
                                // text into the selection. Same for a
                                // completed ask_questions: it draws its Q&A
                                // markdown summary instead of the label.
                                let tool_display_name = self::tool_render::tool_display(&t.tool);
                                let question_summary = tool_display_name == "question"
                                    && self::tool_render::question_markdown(t).is_some();
                                let draws_label = !question_summary
                                    && (tool_display_name != "todo"
                                        || matches!(t.status, crate::types::ToolStatus::Failed(_)));
                                if draws_label && p_top >= vp_top {
                                    let label = self::tool_render::tool_inline_text(t);
                                    text_regions.push(TextRegion {
                                        y1: content_offset,
                                        y2: content_offset + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text: label,
                                    });
                                }
                                // Add visible output lines after the label.
                                // The body mirrors what each tool's renderer
                                // draws (formatted TODO list, extracted diff,
                                // code preview) — never raw JSON schemas.
                                let is_completed =
                                    matches!(t.status, crate::types::ToolStatus::Completed);
                                let is_glob = self::tool_render::tool_display(&t.tool) == "glob";
                                let collapsed_body = |body: String, fallback_id: &str| {
                                    let id = t.tool_call_id.as_deref().unwrap_or(fallback_id);
                                    let collapsed =
                                        crate::util::scroll::collapse_tool_output(&body, 10, 800);
                                    if self.tool_state.is_expanded(id) || !collapsed.overflow {
                                        body
                                    } else {
                                        collapsed.output
                                    }
                                };
                                let display: Option<String> = if is_glob {
                                    let body =
                                        self::tool_render::glob_block_text(t).unwrap_or_default();
                                    if !body.is_empty() && is_completed {
                                        Some(collapsed_body(body, "glob"))
                                    } else {
                                        Some(body)
                                    }
                                } else {
                                    let display_name = self::tool_render::tool_display(&t.tool);
                                    self::tool_render::tool_copy_text(t).map(|body| {
                                        match display_name {
                                            // Read blocks collapse on screen exactly like this.
                                            "read" => collapsed_body(body, "read"),
                                            _ if config.show_tool_details || !is_completed => body,
                                            _ => collapsed_body(body, "shell"),
                                        }
                                    })
                                };
                                if let Some(display) = display.filter(|d| !d.is_empty()) {
                                    let first_output_screen = (p_top.max(vp_top) + 1) as u16;
                                    let screen_end = p_bottom.min(vp_bottom) as u16;
                                    let mut out_screen_y = first_output_screen;
                                    for display_line in display.lines() {
                                        if out_screen_y < screen_end {
                                            let cy = (out_screen_y as i32) - vp_top + scroll;
                                            text_regions.push(TextRegion {
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
                            crate::types::Part::Reasoning(r) => {
                                let expanded = self.tool_state.is_expanded_or(
                                    &r.text[..r.text.floor_char_boundary(32)],
                                    config.thinking_mode,
                                );
                                let header = if expanded { "- Thought" } else { "+ Thought" };
                                if p_top >= vp_top {
                                    text_regions.push(TextRegion {
                                        y1: content_offset,
                                        y2: content_offset + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text: header.to_string(),
                                    });
                                }
                                if expanded && !r.text.is_empty() {
                                    // The body is drawn through the SAME
                                    // markdown pipeline as the screen
                                    // (render_reasoning): word-wrapped, so
                                    // VISUAL rows — not source lines — are
                                    // what a selection must map to. Render
                                    // into the scratch buffer and scan one
                                    // region per visual row, exactly like
                                    // the Summarizing body below. The old
                                    // per-source-line mapping (capped at 10
                                    // lines, blank lines skipped) desynced
                                    // copy regions from the screen, so a
                                    // small selection inside a Thought
                                    // copied nothing or the wrong slice.
                                    let body_w = max_w.saturating_sub(2).max(1);
                                    let content = sanitize_text(&r.text);
                                    let body_h = estimate_height(&content, body_w).max(1);
                                    let scan_area = Rect::new(0, 0, body_w, body_h);
                                    let temp = scratch.get_or_insert_with(|| {
                                        ratatui::buffer::Buffer::empty(scan_area)
                                    });
                                    if *temp.area() != scan_area {
                                        temp.resize(scan_area);
                                    }
                                    let mut md = cosh_tui::core::renderables::markdown::
                                        MarkdownRenderable::new(Some(content));
                                    md.set_fg(Some(ColorInput::RGBA(theme.text_muted)));
                                    md.set_bg(Some(ColorInput::RGBA(theme.background)));
                                    crate::util::markdown::apply_theme(&mut md, theme);
                                    md.render_self(temp, scan_area);

                                    // Body rows start one row below the
                                    // "+ Thought" header. A part may straddle
                                    // the viewport top: skip the rows scrolled
                                    // off above instead of mapping body line 0
                                    // onto the first visible row (that offset
                                    // made small selections near the viewport
                                    // top copy from the wrong line).
                                    let body_top = p_top + 1;
                                    let row_start = (vp_top - body_top).max(0);
                                    let row_end = (p_bottom.min(vp_bottom) - body_top)
                                        .max(0)
                                        .min(i32::from(body_h));
                                    for k in row_start..row_end {
                                        let mut line_text = String::new();
                                        for tx in 0..body_w {
                                            if let Some(cell) = temp.cell((tx, k as u16)) {
                                                line_text.push(
                                                    cell.symbol().chars().next().unwrap_or(' '),
                                                );
                                            }
                                        }
                                        let trimmed = line_text.trim_end().to_string();
                                        let cy = body_top + k - vp_top + scroll;
                                        text_regions.push(TextRegion {
                                            y1: cy,
                                            y2: cy + 1,
                                            x1: x_off + 2,
                                            x2: x_off + max_w,
                                            text: trimmed,
                                        });
                                    }
                                }
                            }
                            crate::types::Part::Compaction(c) => {
                                if !c.text.is_empty() {
                                    // The "Summarizing" box: title row + the
                                    // visible body rows (tail when collapsed,
                                    // full text when expanded). The rows come
                                    // from the RENDERED markdown (laid out into
                                    // the shared scratch buffer and scanned),
                                    // so copy/selection text matches what is
                                    // on screen — never raw md syntax.
                                    let expanded = self.tool_state.is_expanded(&summarizing_id(c));
                                    let mut cy = content_offset;
                                    let mut title = if expanded { "- " } else { "+ " }.to_string();
                                    title.push_str("Summarizing");
                                    if p_top >= vp_top {
                                        text_regions.push(TextRegion {
                                            y1: cy,
                                            y2: cy + 1,
                                            x1: x_off,
                                            x2: x_off + max_w,
                                            text: title,
                                        });
                                    }
                                    cy += 1;
                                    let wrap_w = max_w.saturating_sub(3).max(1);
                                    let body_h = estimate_height(&c.text, wrap_w).max(1);
                                    let (src_start, rows) =
                                        if expanded || body_h <= SUMMARIZING_COLLAPSED_LINES {
                                            (0, body_h)
                                        } else {
                                            (
                                                body_h - SUMMARIZING_COLLAPSED_LINES,
                                                SUMMARIZING_COLLAPSED_LINES,
                                            )
                                        };
                                    let scan_area = Rect::new(0, 0, wrap_w, body_h);
                                    let temp = scratch.get_or_insert_with(|| {
                                        ratatui::buffer::Buffer::empty(scan_area)
                                    });
                                    if *temp.area() != scan_area {
                                        temp.resize(scan_area);
                                    }
                                    let content = sanitize_text(&c.text);
                                    let mut md =
                                        cosh_tui::core::renderables::markdown::MarkdownRenderable::new(
                                            Some(content),
                                        );
                                    md.set_fg(Some(ColorInput::RGBA(theme.text)));
                                    md.set_bg(Some(ColorInput::RGBA(theme.background_panel)));
                                    crate::util::markdown::apply_theme(&mut md, theme);
                                    md.render_self(temp, scan_area);
                                    for k in 0..rows {
                                        let src_y = src_start + k;
                                        let mut line_text = String::new();
                                        for tx in 0..wrap_w {
                                            if let Some(cell) = temp.cell((tx, src_y)) {
                                                line_text.push(
                                                    cell.symbol().chars().next().unwrap_or(' '),
                                                );
                                            }
                                        }
                                        let trimmed = line_text.trim_end().to_string();
                                        text_regions.push(TextRegion {
                                            y1: cy,
                                            y2: cy + 1,
                                            x1: x_off + 2,
                                            x2: x_off + max_w,
                                            text: trimmed,
                                        });
                                        cy += 1;
                                    }
                                } else if p_top >= vp_top {
                                    let text = compaction_line(c, crate::types::now_ms());
                                    text_regions.push(TextRegion {
                                        y1: content_offset,
                                        y2: content_offset + 1,
                                        x1: x_off,
                                        x2: x_off + max_w,
                                        text,
                                    });
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

    /// Content-space row span covered by the current drag selection: the
    /// anchor converted with the scroll offset captured at mouse-down, the
    /// focus with the current one. Returned as (start, end), end exclusive.
    pub fn selection_content_range(&self, anchor_y: u16, focus_y: u16) -> (i32, i32) {
        let vp_top = self.session_area.map_or(0, |(_, y, _, _)| i32::from(y));
        let content_anchor = (i32::from(anchor_y)) - vp_top + self.mouse_down_scroll_y;
        let content_focus = (i32::from(focus_y)) - vp_top + self.scroll_y;
        (
            content_anchor.min(content_focus),
            content_anchor.max(content_focus),
        )
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

        extract_text_in_region(
            &self.text_regions,
            start_content_y,
            end_content_y,
            start_x,
            end_x,
        )
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
        let stream_state = self
            .update_streaming_message_cache(session, inner_area, max_w, config, theme, streaming);
        let height_update =
            self.ensure_height_caches_fresh(session, max_w, config, stream_state.as_ref());

        // Ensure render cache vectors match message count
        let n_msgs = session.messages.len();
        if height_update.full_rebuild {
            self.msg_cache_tokens.clear();
            self.msg_cache_cells.clear();
            self.msg_cache_w.clear();
            self.msg_cache_h.clear();
            self.msg_cache_text_regions.clear();
            self.msg_cache_last_used.clear();
            self.msg_cache_bytes = 0;
        }
        // Only resize when message count actually changed (avoids O(n) fill per frame).
        if n_msgs != self.msg_cache_tokens.len() {
            self.msg_cache_tokens.resize(n_msgs, !0);
            self.msg_cache_cells.resize(n_msgs, None);
            self.msg_cache_w.resize(n_msgs, 0);
            self.msg_cache_h.resize(n_msgs, 0);
            self.msg_cache_text_regions.resize(n_msgs, None);
            self.msg_cache_last_used.resize(n_msgs, 0);
        }

        self.render_frame = self.render_frame.wrapping_add(1);

        let total_height = self.cached_total_height;
        let visible_height = i32::from(inner_area.height);
        let max_scroll = (self.actual_total_height - visible_height).max(0);
        self.scroll_y = self.scroll_y.clamp(0, max_scroll);

        // Cache dimensions for app.rs
        self.total_height = total_height;
        self.visible_height = visible_height;

        // ── Sticky scroll: recalculateBarProps on content size change ─────────
        // Mirrors OpenCode's `recalculateBarProps()` which calls `applyStickyStart`
        // when content size changes and user hasn't manually scrolled.
        if total_height != self.last_content_height {
            if height_update.tool_state_changed {
                // Expand/collapse is a local viewport interaction. Do not apply
                // sticky-to-bottom here; that behavior is reserved for real
                // content growth such as streamed tokens or appended messages.
                self.sync_manual_scroll_state();
            } else {
                self.recalculate_bar_props(total_height, visible_height);
            }
            self.last_content_height = total_height;
        }

        let stream_token = stream_state.as_ref().map_or_else(
            // Not streaming: use the render cache's stored content token
            // for the last message. `msg_cache_tokens[last]` is refreshed
            // by the walk whenever a non-streaming message re-renders
            // (content change or eviction), so an in-place edit of the last
            // message still rebuilds its selection regions — one frame
            // later, without the O(n) per-frame hash this generation used
            // to pay. Streaming ignores it (the streaming cache keeps the
            // streaming message's regions fresh incrementally).
            || self.msg_cache_tokens.last().copied().unwrap_or(0),
            |s| s.raw_len as u64,
        );
        let regions_gen = text_regions_generation(
            session,
            config,
            max_w,
            self.tool_state.version,
            stream_token,
        );
        if regions_gen != self.text_regions_gen {
            let _regions_start = Instant::now();
            self.build_text_regions(session, inner_area, max_w, config, theme);
            self.text_regions_gen = regions_gen;
            log::debug!(
                "[PERF] text_regions: {}us (built)",
                _regions_start.elapsed().as_micros()
            );
        } else if self.render_frame.is_multiple_of(30) {
            // Rate-limited: this fires on EVERY idle frame otherwise.
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
            scrollbar.set_scroll_size(f64::from(self.actual_total_height.max(1)));
            scrollbar.set_viewport_size(f64::from(visible_height));
            scrollbar.set_scroll_position(f64::from(self.scroll_y));
            scrollbar.set_track_color(Some(theme.background.into()));
            scrollbar.set_thumb_color(Some(theme.text_muted.into()));
            scrollbar.render_self(buf, scrollbar_area);
        }

        let vp_top = i32::from(inner_area.y);
        let vp_bottom = i32::from(inner_area.bottom());

        // Start the message walk at the FIRST message that can intersect the
        // viewport (binary search over `prefix_y`, O(log n)) instead of
        // walking the whole timeline from message 0: on sessions with 500+
        // messages the old per-frame O(n) walk — thousands of tight
        // iterations per frame just to skip off-screen messages — was the
        // dominant frame cost even with the render cache hot. The walk below
        // then STOPS once `msg_top` passes the viewport bottom, so the frame
        // cost is O(log n + visible) instead of O(n). Computed BEFORE the
        // `scratch` field borrow below (`find_first_visible` needs `&self`).
        let first_vis = self.find_first_visible(self.scroll_y);
        let walk_start = if self.msg_height_cache.is_empty() {
            i32::from(inner_area.y) - self.scroll_y
        } else {
            // `prefix_y[first_vis]` already accounts for the 1-row gaps
            // before it; the walk's own `idx > 0` gap logic resumes exactly
            // like the full walk would.
            i32::from(inner_area.y) - self.scroll_y + self.prefix_y[first_vis]
        };

        // When the walk stops past the viewport bottom, the remaining
        // (invisible) messages still contribute their cached heights to the
        // total; `prefix_y[n] - prefix_y[idx]` is exactly that remainder.
        let n_msgs = session.messages.len();
        let mut broke_at: Option<usize> = None;

        // Reuse one scratch buffer across all temp renders in this frame (and
        // across frames — `Buffer::resize` keeps the allocation). Removes the
        // per-frame large allocations that accumulated heap churn and
        // fragmented the allocator over long sessions. A direct field borrow
        // keeps `scratch` disjoint from the `tool_state`/cache writes below.
        let scratch = &mut self.scratch;

        let mut y = walk_start;

        for (idx, msg) in session.messages.iter().enumerate().skip(first_vis) {
            // `prefix_y[first_vis]` already includes the 1-row gaps before the
            // first visited message, so the walk adds gaps only AFTER it.
            if idx > first_vis {
                y += 1;
            }

            let msg_h = self.msg_height_cache[idx];
            let msg_top = y;
            if msg_top >= vp_bottom {
                // Nothing below can intersect the viewport — stop the walk.
                broke_at = Some(idx);
                break;
            }
            let msg_bottom = y + msg_h;
            let is_assistant_non_error =
                matches!(msg.role, MessageRole::Assistant) && !msg.id.starts_with("msg-err-");
            let mut render_actual_h = msg_h;

            // Check if message overlaps with viewport (using i32, no u16 wrap)
            if stream_state.is_some() && idx == n_msgs - 1 {
                // ── Streaming last message: blit the incremental cache ──
                // `update_streaming_message_cache` refreshed the cells this
                // frame (before the height cache ran), so the blit is O(visible
                // rows) with no re-parse and no full re-render. Text regions
                // for this message are built from the same cache in
                // `build_text_regions`.
                self.msg_cache_last_used[idx] = self.render_frame;
                if let Some(sc) = self.streaming_msg.as_ref() {
                    let cached_w = sc.width as usize;
                    let cached_h = sc.height;
                    let src_y = (vp_top - msg_top).max(0) as u16;
                    let dst_y = msg_top.max(vp_top) as u16;
                    let vis_h =
                        ((cached_h as i32).min(vp_bottom - msg_top) - src_y as i32).max(0) as u16;
                    let dst_x = inner_area.x;
                    for dy in 0..vis_h {
                        let base = (src_y + dy) as usize * cached_w;
                        let dst_line_y = dst_y + dy;
                        for dx in 0..cached_w {
                            if let Some(dst) = buf.cell_mut((dst_x + dx as u16, dst_line_y)) {
                                *dst = sc.cells[base + dx].clone();
                            }
                        }
                    }
                    render_actual_h = cached_h as i32;
                }
            } else if msg_bottom > vp_top && msg_top < vp_bottom {
                // Recency stamp for the render-cache LRU: any message the user
                // can currently see counts as recently used.
                self.msg_cache_last_used[idx] = self.render_frame;
                let is_top_clipped = msg_top < vp_top;

                if is_top_clipped && is_assistant_non_error {
                    // ── Top-clipped assistant (non-error) ──
                    let is_streaming_msg = idx == session.messages.len() - 1 && streaming;
                    // Bypass the cell cache only when THIS message contains an
                    // active/finishing spinner so its beam advances every frame.
                    // A global "any spinner in the session" check forced every
                    // top-clipped message to re-render fully on every frame
                    // while any tool animated — the main per-frame cost during
                    // agent bursts.
                    let has_active_spinner = msg.parts.iter().enumerate().any(|(pi, part)| {
                        matches!(part, Part::Tool(t)
                            if self.tool_state.tool_spinners
                                .get(&format!(
                                    "{}_{}",
                                    tool_render::tool_display(&t.tool),
                                    pi
                                ))
                                .is_some_and(|s| !s.is_idle()))
                    });
                    let token = msg_content_token(msg, config_tok, max_w, &self.tool_state);
                    let cache_hit = !is_streaming_msg
                        && !has_active_spinner
                        && !msg_has_running_compaction(msg)
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
                        // ── Fall back: temp buffer render (scratch reused across frames) ──
                        let src_y = (vp_top - msg_top) as u16;
                        let dst_y = vp_top as u16;
                        let generous_h =
                            ((msg_h as u16).saturating_add(inner_area.height)).clamp(100, 5000);
                        let full_area = Rect::new(0, 0, inner_area.width, generous_h);
                        let temp = scratch.get_or_insert_with(|| Buffer::empty(full_area));
                        if *temp.area() != full_area {
                            temp.resize(full_area);
                        }
                        // `Buffer::resize` keeps existing cell content, so clear it
                        // before rendering: un-written cells would otherwise show
                        // glyphs from the previous message rendered into this buffer.
                        temp.reset();
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

                        let mut actual_h = Self::render_assistant_message(
                            temp,
                            full_area,
                            msg,
                            theme,
                            &mut self.tool_state,
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
                            // Re-account the entry's byte cost (old subtracted,
                            // new added) and stamp it most-recently-used. The
                            // writes are per-field so the borrow of the reusable
                            // `scratch` buffer stays disjoint.
                            self.msg_cache_bytes =
                                self.msg_cache_bytes
                                    .saturating_sub(Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ));
                            self.msg_cache_tokens[idx] = token;
                            self.msg_cache_w[idx] = inner_area.width;
                            self.msg_cache_h[idx] = ah;
                            self.msg_cache_cells[idx] = Some(cells);
                            self.msg_cache_text_regions[idx] = Some(regions);
                            self.msg_cache_bytes =
                                self.msg_cache_bytes
                                    .saturating_add(Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ));
                            self.msg_cache_last_used[idx] = self.render_frame;
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
                        let temp = scratch.get_or_insert_with(|| Buffer::empty(full_area));
                        if *temp.area() != full_area {
                            temp.resize(full_area);
                        }
                        // `Buffer::resize` keeps existing cell content, so clear it
                        // before rendering: un-written cells would otherwise show
                        // glyphs from the previous message rendered into this buffer.
                        temp.reset();
                        temp.set_style(
                            full_area,
                            Style::default().bg(rgba_color(theme.background)),
                        );

                        match msg.role {
                            MessageRole::User => {
                                let agent_name = msg.agent.as_deref().unwrap_or("default");
                                let agent_color = agent_colors.get(agent_name, &unique_agents);
                                Self::render_user_message(
                                    temp,
                                    full_area,
                                    msg,
                                    theme,
                                    agent_color,
                                    &mut self.tool_state,
                                    config,
                                    false,
                                    false,
                                    Some(&self.part_heights_cache[idx]),
                                    streaming,
                                );
                            }
                            MessageRole::Assistant => {
                                Self::render_assistant_message(
                                    temp,
                                    full_area,
                                    msg,
                                    theme,
                                    &mut self.tool_state,
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
                        let token = msg_content_token(msg, config_tok, max_w, &self.tool_state);
                        let cache_hit = !is_streaming_msg
                            && !msg_has_running_compaction(msg)
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
                                &mut self.tool_state,
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
                                self.msg_cache_bytes = self.msg_cache_bytes.saturating_sub(
                                    Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ),
                                );
                                self.msg_cache_tokens[idx] = token;
                                self.msg_cache_w[idx] = inner_area.width;
                                self.msg_cache_h[idx] = ah;
                                self.msg_cache_cells[idx] = Some(cells);
                                self.msg_cache_text_regions[idx] = Some(regions);
                                self.msg_cache_bytes = self.msg_cache_bytes.saturating_add(
                                    Self::cache_entry_bytes_of(
                                        &self.msg_cache_cells[idx],
                                        &self.msg_cache_text_regions[idx],
                                    ),
                                );
                                self.msg_cache_last_used[idx] = self.render_frame;
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
                                    &mut self.tool_state,
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
                                let token =
                                    msg_content_token(msg, config_tok, max_w, &self.tool_state);
                                let cache_hit = !is_streaming_msg
                                    && !msg_has_running_compaction(msg)
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
                                        &mut self.tool_state,
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
                                    let temp =
                                        scratch.get_or_insert_with(|| Buffer::empty(full_area));
                                    if *temp.area() != full_area {
                                        temp.resize(full_area);
                                    }
                                    // `Buffer::resize` keeps existing cell content, so clear it
                                    // before rendering: un-written cells would otherwise show
                                    // glyphs from the previous message rendered into this buffer.
                                    temp.reset();
                                    temp.set_style(
                                        full_area,
                                        Style::default().bg(rgba_color(theme.background)),
                                    );

                                    let mut actual_h = Self::render_assistant_message(
                                        temp,
                                        full_area,
                                        msg,
                                        theme,
                                        &mut self.tool_state,
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
                                        self.msg_cache_bytes = self.msg_cache_bytes.saturating_sub(
                                            Self::cache_entry_bytes_of(
                                                &self.msg_cache_cells[idx],
                                                &self.msg_cache_text_regions[idx],
                                            ),
                                        );
                                        self.msg_cache_tokens[idx] = token;
                                        self.msg_cache_w[idx] = inner_area.width;
                                        self.msg_cache_h[idx] = ah;
                                        self.msg_cache_cells[idx] = Some(cells);
                                        self.msg_cache_text_regions[idx] = Some(regions);
                                        self.msg_cache_bytes = self.msg_cache_bytes.saturating_add(
                                            Self::cache_entry_bytes_of(
                                                &self.msg_cache_cells[idx],
                                                &self.msg_cache_text_regions[idx],
                                            ),
                                        );
                                        self.msg_cache_last_used[idx] = self.render_frame;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            y += render_actual_h;
        }

        // Record actual rendered total height (accurate, for scrollbar sizing).
        // Do NOT inflate cached_total_height from the render scan — the cache
        // is the stable source of truth for height estimates. Inflating it would
        // cause total_height != last_content_height on the next frame, triggering
        // recalculate_bar_props which changes scroll_y and creates visible
        // scroll jumps mid-stream.
        //
        // When the walk stopped past the viewport bottom, the invisible
        // messages below it never render (render_actual_h stays = msg_h), so
        // they contribute exactly their cached heights + gaps: the prefix-y
        // span from the stop index to the end. The messages before the stop
        // already contributed their real (possibly diverging) heights to `y`.
        let actual_total = match broke_at {
            Some(idx) => (y - (vp_top - self.scroll_y))
                .saturating_add(self.prefix_y[n_msgs] - self.prefix_y[idx]),
            None => y - (vp_top - self.scroll_y),
        };
        self.actual_total_height = actual_total;
        self.total_height = self.cached_total_height;
        self.last_content_height = self.cached_total_height;

        // ── Hover highlight for user messages (opencode-style) ────────────────
        self.render_hover_highlight(buf, inner_area, session, theme);

        if let Some((anchor_x, _anchor_screen_y, focus_x, _focus_screen_y)) = self.drag_selection
            && inner_area.height > 0
        {
            // Convert content-space anchor and focus to current screen position.
            // Both are stored in content space so the visual highlight follows
            // content during auto-scroll.
            //
            // A zero-height viewport (e.g. the session area briefly collapsing
            // to 0 rows during a terminal resize) has no visible rows to
            // highlight, and the clamp bound `vp_top + height - 1` would fall
            // below `vp_top` — panicking with `min > max`. Skip it entirely.
            let vp_top = i32::from(inner_area.y);
            let vp_bottom = vp_top + i32::from(inner_area.height) - 1;
            let anchor_screen_y = (self.selection_anchor_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_bottom) as u16;
            let focus_screen_y = (self.selection_focus_content_y - self.scroll_y + vp_top)
                .clamp(vp_top, vp_bottom) as u16;

            let start_y = anchor_screen_y.min(focus_screen_y);
            let end_y = anchor_screen_y.max(focus_screen_y);

            // Direction and row bands are matched against CONTENT rows, not
            // the clamped screen rows (mirrors `get_text_in_region`): an
            // endpoint whose row scrolled OUT of the viewport is clamped to
            // the edge for the iteration bounds, but that edge row is then a
            // MIDDLE row of the selection and must be highlighted full-width.
            // Matching clamped screen rows instead froze the initial
            // mouse-down x on the first/last visible line for the whole drag
            // (e.g. an upward drag past the top edge kept the partial band on
            // the last line).
            let anchor_content_y = self.selection_anchor_content_y;
            let focus_content_y = self.selection_focus_content_y;
            let (top_x, bottom_x) = if anchor_content_y <= focus_content_y {
                (anchor_x, focus_x)
            } else {
                (focus_x, anchor_x)
            };
            let top_content_y = anchor_content_y.min(focus_content_y);
            let bottom_content_y = anchor_content_y.max(focus_content_y);

            let content_min_x = inner_area.x + 3;
            let content_max_x = (inner_area.x + 3 + max_w).saturating_sub(1);

            for cy in start_y..=end_y {
                // Content row under this screen row. Rows outside the true
                // content span (selection fully clamped off-screen) are not
                // part of the selection — skip them.
                let row_content_y = i32::from(cy) - vp_top + self.scroll_y;
                if row_content_y < top_content_y || row_content_y > bottom_content_y {
                    continue;
                }
                let (lx1, lx2) =
                    if row_content_y == top_content_y && row_content_y == bottom_content_y {
                        (top_x.min(bottom_x), top_x.max(bottom_x))
                    } else if row_content_y == top_content_y {
                        (top_x, content_max_x)
                    } else if row_content_y == bottom_content_y {
                        (content_min_x, bottom_x)
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

        // ── Bounded process memory on long sessions ──────────────────────────
        // The render cache is the fattest in-memory copy (up to ~40 bytes per
        // rendered cell). Once it exceeds the generous budget, shed the oldest
        // entries outside the viewport guard; idle tool spinners (never
        // rendered) are dropped every frame so the map stays bounded by the
        // number of tools currently animating.
        if self.msg_cache_bytes > RENDER_CACHE_BUDGET {
            self.prune_render_cache(vp_top, vp_bottom);
        }
        self.tool_state.sweep_idle_spinners();

        self.handle_auto_scroll(delta_time, total_height, visible_height);

        let _frame_us = _frame_start.elapsed().as_micros();
        // Rate-limited: during sustained slow phases (e.g. streaming a huge
        // message) this would otherwise fire on every frame. Mildly-slow
        // frames (>5ms) are sampled at 1/30; genuinely bad frames (>50ms) are
        // always logged so an isolated freeze is never silently missed.
        if _frame_us > 5000 && (_frame_us > 50_000 || self.render_frame.is_multiple_of(30)) {
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

    fn handle_auto_scroll(&mut self, delta_time: f64, _total_height: i32, visible_height: i32) {
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
            let max_scroll = (self.actual_total_height - visible_height).max(0);
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
