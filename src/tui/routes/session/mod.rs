pub mod footer;
pub mod free_gateway_recommendation;
pub mod streaming;
pub mod tool_render;

pub mod agents_split;
pub mod agent_command_map;

pub mod delete;
pub mod permission;
pub mod question;
pub mod queue_choice;
pub mod right_panel;
pub mod subagent_footer;
#[cfg(test)]
mod tests;

mod auto_scroll;
mod height_caches;
mod measure;
mod message_layout;
mod mouse;
mod msg_tokens;
mod part_lines;
mod pill;
mod reasoning;
mod render;
mod render_cache;
mod render_parts;
mod scroll;
mod text;
mod text_regions;

// Private re-imports: the sibling submodules (and `tests.rs`) reach these
// names through `super::`, exactly as they did while they lived here.
use msg_tokens::{
    RENDER_CACHE_BUDGET, RENDER_CACHE_GUARD_ROWS, RENDER_CACHE_LOW_WATER, msg_change_token,
    msg_content_token, msg_has_running_compaction, msg_has_running_install, mutable_msg_indices,
    status_to_u8,
};
use part_lines::{
    SUMMARIZING_COLLAPSED_LINES, SUMMARIZING_PAD_V, compaction_color, compaction_line, install_line,
    summarizing_height, summarizing_id,
};
use text::{blit_cell_rows, conceal_text, draw_hint_button, hash_parts, sanitize_text};

use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use self::tool_render::blank_border_chars;
use cosh_tui::core::lib::border::BorderSidesConfig;
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
use crate::component::scroll_pill::ScrollPill;
use crate::types::{
    AgentColors, CompactionPart, FilePart, InstallPart, Message, MessageRole, Part, ReasoningPart,
    SessionStatus, ToolPart, ToolStatus,
};
use std::time::Instant;




use crate::util::draw::draw_text_line;
use crate::util::text_region::{
    TextRegion, extract_text_in_region, regions_from_full_width_cells, text_from_cell_row,
};

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

    // ── Scroll-to-bottom pill (jump back to the live content) ──────────────────
    /// Shared `⮟` pill component (see `component::scroll_pill`). The mouse
    /// dispatcher hit-tests `pill.area`, so a click always lands on exactly
    /// what was drawn. Kept `pub` for the dispatcher/tests.
    pub pill: ScrollPill,

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
    /// Reusable buffer for `tool_spinners` map keys. The per-message
    /// spinner check used to `format!` a fresh `String` for every tool part
    /// of every visible message on every frame; reusing one allocation keeps
    /// the scroll-path lookup allocation-free.
    spinner_key_scratch: String,
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
    /// Id of the message the streaming cache last rendered. Kept after the
    /// message stops being the session's last one (a queued user message was
    /// injected below it): the height cache must keep re-estimating it while
    /// deltas still arrive, or its frozen estimate clips the painted content
    /// (the "block cut" symptom).
    last_streamed_msg: Option<String>,
    /// Caches for the width we are NOT currently rendering at (see
    /// [`WidthCacheSnapshot`]): parked when a full rebuild leaves one width and
    /// swapped back O(1) when the toggle returns — never cloned.
    width_cache_snapshot: Option<Box<WidthCacheSnapshot>>,
}

#[derive(Clone, Copy, Default)]
struct HeightCacheUpdate {
    full_rebuild: bool,
    tool_state_changed: bool,
}

/// Who is asking [`SessionView::ensure_height_caches_fresh`] to run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeightCacheMode {
    /// The renderer: caches may be rebuilt, extended and re-estimated freely.
    Render,
    /// Mouse hit-testing (`update_hover` / `handle_mouse`): the cursor must
    /// be mapped against the layout the last render PAINTED. When the caches
    /// are structurally fresh (same session/width/config/tool version) the
    /// incremental content scan is skipped entirely — re-estimating here
    /// would mutate the shared caches between frames and desynchronize the
    /// hover/click geometry from the screen (the highlight used to jump onto
    /// the block above a queued message during active streaming). Structural
    /// staleness (session switch, width/config change, message count grew)
    /// still rebuilds: the old layout cannot be mapped onto the new content.
    PaintedOnly,
}

/// A standby copy of the width-dependent render caches, kept so a left-panel
/// toggle back to a previously-used width restores that width's exact caches
/// instead of re-estimating every message height and re-rendering every
/// visible message from scratch. On a 10-hour transcript that full rebuild was
/// the ~1s toggle frame that froze the header animation.
///
/// Only the caches that actually depend on line width are captured — the
/// scroll position, stream cache, hover/pill state, and the reusable scratch
/// buffers stay on the view. Restoring the set is a battery of O(1) `Vec`
/// moves (the heavy cells are never cloned), which is what makes the return
/// toggle a cache-hit frame rather than a reflow.
///
/// A snapshot is only valid for the exact (session, config, tool-expansion,
/// message-count, width) it was captured under; [`WidthCacheSnapshot::swap_with_view`]
/// re-validates those keys in O(1) and the caller discards the snapshot if any
/// of them drifted.
struct WidthCacheSnapshot {
    /// Validity keys — compared O(1) at swap time so a stale snapshot is
    /// dropped instead of ever being trusted.
    session_id: String,
    /// Number of messages this snapshot's vectors cover.
    msg_len: usize,
    /// Config token this snapshot's caches were built under — tracks the
    /// view's `cache_config_token`, not the current one, so a config change
    /// only invalidates the snapshot if the config is still different when we
    /// try to swap it back in.
    config_tok: u64,
    tool_state_version: u64,
    /// Width this snapshot's heights were estimated for.
    cache_max_w: u16,

    // ── Width-dependent height caches ────────────────────────────────────────
    msg_height_cache: Vec<i32>,
    prefix_y: Vec<i32>,
    part_heights_cache: Vec<Vec<u16>>,
    last_msg_change_token: u64,
    msg_change_tokens: Vec<u64>,
    prev_mutable_msgs: Vec<usize>,
    cached_total_height: i32,
    actual_total_height: i32,
    last_content_height: i32,

    // ── Width-dependent per-message render caches ────────────────────────────
    msg_cache_tokens: Vec<u64>,
    msg_cache_cells: Vec<Option<Vec<ratatui::buffer::Cell>>>,
    msg_cache_w: Vec<u16>,
    msg_cache_h: Vec<u16>,
    msg_cache_text_regions: Vec<Option<Vec<TextRegion>>>,
    msg_cache_last_used: Vec<u64>,
    msg_cache_bytes: usize,
    /// Generation stamp matching `text_regions` — the region build is skipped
    /// when the restored generation equals the freshly-computed one.
    text_regions_gen: u64,
    /// Selection regions for the captured width — swapped alongside `text_regions_gen`
    /// because leaving the view's current-width regions while restoring the
    /// old width's stamp would suppress a needed rebuild and break selection.
    text_regions: Vec<TextRegion>,
}

impl WidthCacheSnapshot {
    /// Lift the view's currently-active width-dependent caches out of it, in
    /// place of a full rebuild's `clear()`. The view keeps every field the
    /// rebuild path rewrites immediately after (`cache_max_w`,
    /// `cached_total_height`, `actual_total_height`, `last_content_height`),
    /// so the caller continues exactly as it would without the snapshot.
    fn capture(view: &mut SessionView) -> Self {
        Self {
            session_id: view.last_session_id.clone().unwrap_or_default(),
            msg_len: view.msg_height_cache.len(),
            config_tok: view.cache_config_token,
            tool_state_version: view.last_tool_state_version,
            cache_max_w: view.cache_max_w,
            msg_height_cache: std::mem::take(&mut view.msg_height_cache),
            prefix_y: std::mem::take(&mut view.prefix_y),
            part_heights_cache: std::mem::take(&mut view.part_heights_cache),
            last_msg_change_token: view.last_msg_change_token,
            msg_change_tokens: std::mem::take(&mut view.msg_change_tokens),
            prev_mutable_msgs: std::mem::take(&mut view.prev_mutable_msgs),
            cached_total_height: view.cached_total_height,
            actual_total_height: view.actual_total_height,
            last_content_height: view.last_content_height,
            msg_cache_tokens: std::mem::take(&mut view.msg_cache_tokens),
            msg_cache_cells: std::mem::take(&mut view.msg_cache_cells),
            msg_cache_w: std::mem::take(&mut view.msg_cache_w),
            msg_cache_h: std::mem::take(&mut view.msg_cache_h),
            msg_cache_text_regions: std::mem::take(&mut view.msg_cache_text_regions),
            msg_cache_last_used: std::mem::take(&mut view.msg_cache_last_used),
            msg_cache_bytes: std::mem::take(&mut view.msg_cache_bytes),
            text_regions_gen: view.text_regions_gen,
            text_regions: std::mem::take(&mut view.text_regions),
        }
    }

    /// O(1) swap: the snapshot's caches become the view's active set, and the
    /// view's outgoing set is parked back in the snapshot — so the next toggle
    /// is a swap in the other direction. The caller MUST validate the keys
    /// first; invariants like "active `msg_height_cache.len()` equals every
    /// render-cache vector's length and `prefix_y` is length+1" survive the
    /// swap exactly because both sets were internally consistent on their own.
    fn swap_with_view(&mut self, view: &mut SessionView) {
        std::mem::swap(&mut self.msg_height_cache, &mut view.msg_height_cache);
        std::mem::swap(&mut self.prefix_y, &mut view.prefix_y);
        std::mem::swap(&mut self.part_heights_cache, &mut view.part_heights_cache);
        std::mem::swap(&mut self.msg_change_tokens, &mut view.msg_change_tokens);
        std::mem::swap(&mut self.prev_mutable_msgs, &mut view.prev_mutable_msgs);
        std::mem::swap(&mut self.msg_cache_tokens, &mut view.msg_cache_tokens);
        std::mem::swap(&mut self.msg_cache_cells, &mut view.msg_cache_cells);
        std::mem::swap(&mut self.msg_cache_w, &mut view.msg_cache_w);
        std::mem::swap(&mut self.msg_cache_h, &mut view.msg_cache_h);
        std::mem::swap(
            &mut self.msg_cache_text_regions,
            &mut view.msg_cache_text_regions,
        );
        std::mem::swap(&mut self.msg_cache_last_used, &mut view.msg_cache_last_used);
        std::mem::swap(&mut self.text_regions, &mut view.text_regions);
        // Scalars that travel with the set. `config_tok ↔ cache_config_token`
        // keeps each set keyed by the config it was built under; the session /
        // tool-expansion keys were validated equal to the current ones, so the
        // snapshot is re-keyed to them after the swap below.
        std::mem::swap(&mut self.cache_max_w, &mut view.cache_max_w);
        std::mem::swap(&mut self.config_tok, &mut view.cache_config_token);
        std::mem::swap(&mut self.cached_total_height, &mut view.cached_total_height);
        std::mem::swap(&mut self.actual_total_height, &mut view.actual_total_height);
        std::mem::swap(&mut self.last_content_height, &mut view.last_content_height);
        std::mem::swap(&mut self.text_regions_gen, &mut view.text_regions_gen);
        std::mem::swap(
            &mut self.last_msg_change_token,
            &mut view.last_msg_change_token,
        );
        std::mem::swap(&mut self.msg_cache_bytes, &mut view.msg_cache_bytes);
        // Re-key what the snapshot now holds (the view's erstwhile active set)
        // to describe IT: same session and tool-expansion version (validated),
        // with its own width/config/msg-count coming from the swapped fields.
        self.session_id = view.last_session_id.clone().unwrap_or_default();
        self.msg_len = self.msg_height_cache.len();
        self.tool_state_version = view.last_tool_state_version;
    }
}

/// Push one sampled scratch row as a content-only [`TextRegion`].
///
/// Box renderers pad their content with leading blank columns (box margin,
/// internal padding, gutters), so a row sampled straight from the painted
/// cells starts with spaces before its first glyph. The content-only contract
/// requires `text` char 0 to map to screen column `x1`, so the leading blank
/// run is trimmed and the region is labeled `x1 = x_off + indent` — interior
/// indentation is untouched, and the copy equals exactly the visible glyphs.
/// Whitespace-only rows contribute no region.
fn push_sampled_row(
    text_regions: &mut Vec<TextRegion>,
    cy: i32,
    x_off: u16,
    max_w: u16,
    raw: &str,
) {
    let trimmed = raw.trim_end();
    if trimmed.is_empty() {
        return;
    }
    // Leading whitespace is blank paint (box margin, padding, gutter): strip it
    // from the text and advance x1 by the same CHAR count so text char 0 keeps
    // mapping to screen column x1. Counted in chars (not bytes) so multi-byte
    // whitespace would still advance the label by the right number of columns.
    // Interior indentation is untouched.
    let indent = trimmed.chars().take_while(|c| c.is_whitespace()).count();
    let x1 = x_off
        .saturating_add(indent as u16)
        .min(x_off.saturating_add(max_w));
    let content: String = trimmed.chars().skip(indent).collect();
    text_regions.push(TextRegion::one_row(
        cy,
        x1,
        x_off.saturating_add(max_w),
        content,
    ));
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
