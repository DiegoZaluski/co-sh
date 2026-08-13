/// Maximum number of PTY sessions (bash + subagent) kept in the right panel.
/// Older sessions are evicted first so the panel cannot grow without bound
/// over the process lifetime (restarting the app used to be the only way to
/// clear the accumulated output).
const MAX_PTY_SESSIONS: usize = 20;

/// Maximum number of characters of output retained per PTY session.
/// Only the TAIL is kept, since the panel renders the most recent output.
const MAX_PTY_OUTPUT_CHARS: usize = 60_000;

use std::time::{Duration, Instant};

use cosh_tui::core::renderables::markdown::estimate_height;
use ratatui::buffer::{Buffer, Cell};

use crate::util::text_region::{TextRegion, extract_text_in_region};

/// Minimum gap between subagent layout rebuilds (heights + rendered body
/// cells). During streaming, chunks arrive far more often than the eye can
/// track; rebuilding the whole accumulated body on EVERY chunk re-parses up
/// to 60k chars per frame and stalls the TUI (especially in debug builds).
/// Rebuilds are coalesced to at most one per interval — frames in between
/// serve the previous cells, stale by less than one interval, which is
/// invisible in a streaming panel.
pub(crate) const SUBAGENT_REBUILD_INTERVAL: Duration = Duration::from_millis(100);

/// Split a subagent PTY output into the optional main-agent input line
/// (`→ cosh: ...`, prepended by `app.rs` when the tool call carries an
/// `input`) and the remaining subagent body. When the output was truncated
/// (tail-only retention) the prefix may be gone — then everything is body.
pub(crate) fn split_subagent_output(output: &str) -> (Option<&str>, &str) {
    if !output.starts_with("→ cosh:") {
        return (None, output);
    }
    match output.find('\n') {
        Some(i) => (Some(&output[..i]), &output[i + 1..]),
        None => (Some(output), ""),
    }
}

/// Filter control characters (keeping `\n`) so raw bytes can never reach
/// ratatui buffer cells (cell_width panic). Same contract as the chat's
/// markdown rendering — both the height estimate and the render use the
/// sanitized body so they stay in lockstep.
pub(crate) fn sanitize_subagent_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect()
}

/// Cached rendered markdown cells of one subagent body. Frames between
/// streamed chunks blit these cells instead of re-running the markdown
/// renderer over the whole (up to 60k-char) body every frame.
#[derive(Debug, Clone)]
pub(crate) struct SubagentBodyCache {
    /// Session id the entry belongs to — entries are matched by id, not
    /// index, so session eviction never serves stale cells.
    pub(crate) id: String,
    /// `subagent_layout_gen` the cells were rendered for — the shared layout
    /// generation that keeps heights and bodies consistent (see
    /// [`RightPanelState::subagent_layout_gen`]).
    pub(crate) layout_gen: u64,
    /// Wrap width the cells were laid out at.
    pub(crate) wrap_w: u16,
    /// Body rows (`cells.len() == wrap_w as usize * h as usize`).
    pub(crate) h: u16,
    /// Theme colors the cells were styled with (fg + box bg) — the palette
    /// derives from these, so a live theme switch must invalidate.
    pub(crate) theme_key: u64,
    /// Row-major rendered cells, `wrap_w` × `h`.
    pub(crate) cells: Vec<Cell>,
}

/// A single todo item from the LLM's plan_todo_write tool.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub status: String,
    pub content: String,
}

/// Status of a PTY session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyStatus {
    Running,
    Completed,
    Failed,
}

/// A PTY session representing a bash command run by the LLM.
#[derive(Debug, Clone)]
pub struct PtySession {
    pub id: String,
    pub command: String,
    pub output: String,
    pub workdir: Option<String>,
    pub status: PtyStatus,
}

/// Identifies which section of the right panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Todo,
    Bash,
    Subagent,
}

pub(crate) const fn section_kind_index(kind: SectionKind) -> usize {
    match kind {
        SectionKind::Todo => 0,
        SectionKind::Bash => 1,
        SectionKind::Subagent => 2,
    }
}

/// Screen-space layout of one visible right-panel section, populated on every
/// render so mouse events can resolve which section the cursor is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionLayout {
    pub kind: SectionKind,
    /// First screen row of the section band (inclusive).
    pub top: i32,
    /// Last screen row of the section band (exclusive).
    pub bottom: i32,
    /// First screen row where the section's CONTENT is drawn (box top +
    /// internal top padding). Maps a cursor row to content rows via
    /// `content_row = (screen_y - content_top) + section_scroll`.
    pub content_top: i32,
}

/// Distance from a section edge (in rows) that triggers drag auto-scroll.
const AUTO_SCROLL_THRESHOLD: i32 = 3;
/// Drag auto-scroll speeds (rows/sec), mirroring the chat's values.
const AUTO_SCROLL_SPEED_SLOW: f64 = 6.0;
const AUTO_SCROLL_SPEED_MEDIUM: f64 = 36.0;
const AUTO_SCROLL_SPEED_FAST: f64 = 72.0;

/// State for the right panel.
#[derive(Debug, Clone)]
pub struct RightPanelState {
    /// Current list of TODOs.
    pub todos: Vec<TodoItem>,
    /// Active/completed PTY sessions.
    pub pty_sessions: Vec<PtySession>,
    /// Counter for matching ToolResult back to plan_todo_write tool calls.
    pub pending_todo_update_count: u32,
    /// Counter for generating unique PTY IDs.
    next_pty_id: u64,
    /// Monotonically increasing counter for section activation order.
    /// Higher value = more recently activated.
    next_activity_id: u64,
    /// Current PTY mutation generation. Bumped on every output change; used
    /// to invalidate the derived caches (heights + rendered bodies).
    pub(crate) pty_gen: u64,
    /// Per-section activation order values. Index by `section_kind_index()`.
    section_activity_order: [u64; 3],

    // ── Derived buffer caches (avoid rebuilding all accumulated output per frame) ──
    /// Cached bash line buffer (`$ command` + output lines), keyed to `pty_gen`.
    bash_buffer_cache: Vec<String>,
    bash_cache_gen: u64,
    /// Cached subagent line buffer (command header + output lines), keyed to `pty_gen`.
    subagent_buffer_cache: Vec<String>,
    subagent_cache_gen: u64,

    // ── Per-section scroll state ───────────────────────────────────
    /// Scroll offset within the TODO section (only when items overflow).
    pub todo_scroll_y: i32,
    /// Scroll offset within the bash section (shared across all bash PTYs).
    pub bash_scroll_y: i32,
    /// Scroll offset within the subagent section (shared across all subagent PTYs).
    pub subagent_scroll_y: i32,

    // ── Subagent markdown layout cache ────────────────────────────
    /// Cached per-session content rows (command header + optional input
    /// line + markdown body rows) for the subagent section, keyed to
    /// `subagent_layout_gen` and the wrap width. Rebuilt only when output or
    /// width changes — `estimate_height` (a pulldown_cmark parse) is too
    /// expensive to run on every frame.
    subagent_rows_cache: Vec<u16>,
    subagent_rows_cache_w: u16,
    /// The `pty_gen` the subagent layout caches (rows + rendered body cells)
    /// were last rebuilt for. When `pty_gen` moves ahead but the rebuild is
    /// throttled, BOTH caches keep serving their previous values, so heights
    /// and bodies always agree — they are never rebuilt on different gens.
    pub(crate) subagent_layout_gen: u64,
    /// When the last subagent layout rebuild happened — rebuilds are
    /// coalesced to at most one per [`Self::subagent_rebuild_interval`].
    last_subagent_rebuild: Instant,
    /// Minimum gap between subagent layout rebuilds (default
    /// [`SUBAGENT_REBUILD_INTERVAL`]). Tests set this to `Duration::ZERO`
    /// to disable the throttle.
    pub(crate) subagent_rebuild_interval: Duration,
    /// Cached RENDERED cells of each subagent body, keyed to the session id,
    /// the shared layout generation, the wrap width and the theme colors
    /// (see [`SubagentBodyCache`]). Frames between streamed chunks — the
    /// common case — blit these cells instead of re-parsing and re-rendering
    /// the whole body, which was the right-panel frame bottleneck. Entries
    /// are matched by session id, so evicted sessions never serve stale
    /// cells.
    pub(crate) subagent_body_cache: Vec<Option<SubagentBodyCache>>,
    /// Reusable scratch buffer for blitting the visible slice of a session's
    /// markdown body: the markdown renderer lays out from content row 0, so
    /// the full body is rendered here, copied into the body cache, and only
    /// the visible rows are copied into the panel. Kept across frames —
    /// `Buffer::resize` retains the allocation, so streaming never allocates
    /// a large buffer per frame.
    pub(crate) subagent_scratch: Option<Buffer>,

    // ── Auto-scroll tracking ────────────────────────────────────────
    /// Set to `true` when the user manually scrolls (up/down);
    /// set to `false` by `scroll_to_bottom()`. Used by `is_scrolled_up()`
    /// to decide whether to auto-scroll on new output.
    pub user_scrolled_away: bool,

    // ── Section layout (populated on render, consumed by mouse events) ──
    /// Screen bounds of each visible section from the last render. Cleared
    /// when the panel is hidden. Mouse wheel/keyboard scroll and drag
    /// selection resolve the target section through this.
    pub section_layouts: Vec<SectionLayout>,

    // ── Drag selection (bash/subagent) ──────────────────────────────
    /// Screen-space selection rectangle (anchor x, anchor y, focus x, focus y).
    pub drag_selection: Option<(u16, u16, u16, u16)>,
    /// Section the current drag selection belongs to.
    pub selection_section: Option<SectionKind>,
    /// Anchor/focus in the section's CONTENT rows (set at mouse-down / on
    /// each drag), so the highlight follows content during auto-scroll.
    pub selection_anchor_content_y: i32,
    pub selection_focus_content_y: i32,
    /// The section's scroll offset when the drag started.
    pub selection_mouse_down_scroll_y: i32,
    /// Whether a selection drag is auto-scrolling near a section edge.
    pub is_auto_scrolling: bool,
    auto_scroll_accumulator: f64,
    auto_scroll_speed: f64,

    // ── Text regions (selection extraction) ─────────────────────────
    /// One region per bash content row (`bash_buffer()` line). Keyed to
    /// `pty_gen` + width via `text_regions_gen`/`text_regions_w`.
    pub(crate) bash_text_regions: Vec<TextRegion>,
    /// One region per subagent content row (header + input + body rows).
    pub(crate) subagent_text_regions: Vec<TextRegion>,
    /// `pty_gen` the text regions were last rebuilt for.
    pub(crate) text_regions_gen: u64,
    /// Wrap width the subagent regions were laid out at.
    pub(crate) text_regions_w: u16,

    // ── Legacy (kept for external consumers) ───────────────────────
    pub scroll_y: i32,
    pub content_height: i32,
    pub visible_height: i32,
}

impl RightPanelState {
    pub fn new() -> Self {
        Self {
            todos: Vec::new(),
            pty_sessions: Vec::new(),
            pending_todo_update_count: 0,
            next_pty_id: 0,
            next_activity_id: 1,
            section_activity_order: [0; 3],
            pty_gen: 0,
            subagent_body_cache: Vec::new(),
            bash_buffer_cache: Vec::new(),
            bash_cache_gen: 0,
            subagent_buffer_cache: Vec::new(),
            subagent_cache_gen: 0,
            todo_scroll_y: 0,
            bash_scroll_y: 0,
            subagent_scroll_y: 0,
            subagent_rows_cache: Vec::new(),
            subagent_rows_cache_w: 0,
            subagent_layout_gen: 0,
            last_subagent_rebuild: Instant::now()
                .checked_sub(SUBAGENT_REBUILD_INTERVAL)
                .unwrap_or_else(Instant::now),
            subagent_rebuild_interval: SUBAGENT_REBUILD_INTERVAL,
            subagent_scratch: None,
            user_scrolled_away: false,
            section_layouts: Vec::new(),
            drag_selection: None,
            selection_section: None,
            selection_anchor_content_y: 0,
            selection_focus_content_y: 0,
            selection_mouse_down_scroll_y: 0,
            is_auto_scrolling: false,
            auto_scroll_accumulator: 0.0,
            auto_scroll_speed: 0.0,
            bash_text_regions: Vec::new(),
            subagent_text_regions: Vec::new(),
            text_regions_gen: 0,
            text_regions_w: 0,
            scroll_y: 0,
            content_height: 0,
            visible_height: 0,
        }
    }

    /// Mark a section as recently activated. The section with the highest
    /// activity value is rendered at the top of the panel.
    pub fn mark_activity(&mut self, kind: SectionKind) {
        let idx = section_kind_index(kind);
        self.section_activity_order[idx] = self.next_activity_id;
        self.next_activity_id += 1;
    }

    /// Activity order value for a given section kind. 0 = never activated.
    pub fn section_activity(&self, kind: SectionKind) -> u64 {
        let idx = section_kind_index(kind);
        self.section_activity_order[idx]
    }

    // ── Section layout (for cursor-based targeting) ──────────────────
    /// Reset the section layout list (call when the panel is hidden).
    pub fn clear_section_layouts(&mut self) {
        self.section_layouts.clear();
    }

    /// Record the layout of one visible section (called by the renderer).
    pub fn push_section_layout(&mut self, kind: SectionKind, top: i32, bottom: i32) {
        let content_top = top + 2; // TOP_GAP (1) + TOP_PAD (1), same for every section
        self.section_layouts.push(SectionLayout {
            kind,
            top,
            bottom,
            content_top,
        });
    }

    /// The section whose band contains screen row `y`, if any.
    pub fn section_at(&self, y: u16) -> Option<SectionKind> {
        let y = i32::from(y);
        self.section_layouts
            .iter()
            .find(|l| y >= l.top && y < l.bottom)
            .map(|l| l.kind)
    }

    fn section_layout(&self, kind: SectionKind) -> Option<&SectionLayout> {
        self.section_layouts.iter().find(|l| l.kind == kind)
    }

    fn section_scroll(&self, kind: SectionKind) -> i32 {
        match kind {
            SectionKind::Todo => self.todo_scroll_y,
            SectionKind::Bash => self.bash_scroll_y,
            SectionKind::Subagent => self.subagent_scroll_y,
        }
    }

    fn set_section_scroll(&mut self, kind: SectionKind, value: i32) {
        match kind {
            SectionKind::Todo => self.todo_scroll_y = value,
            SectionKind::Bash => self.bash_scroll_y = value,
            SectionKind::Subagent => self.subagent_scroll_y = value,
        }
    }

    /// Scroll ONLY the section under screen row `y`. Returns `false` when no
    /// section owns that row (e.g. the panel is hidden), leaving the offset
    /// untouched.
    pub fn scroll_up_at(&mut self, y: u16, delta: i32) -> bool {
        let Some(kind) = self.section_at(y) else {
            return false;
        };
        self.scroll_section_by(kind, -delta);
        true
    }

    /// Scroll ONLY the section under screen row `y`. Returns `false` when no
    /// section owns that row (e.g. the panel is hidden), leaving the offset
    /// untouched.
    pub fn scroll_down_at(&mut self, y: u16, delta: i32) -> bool {
        let Some(kind) = self.section_at(y) else {
            return false;
        };
        self.scroll_section_by(kind, delta);
        true
    }

    /// Apply a signed delta to one section's scroll offset.
    ///
    /// Uses saturating arithmetic: the stored offsets may hold the `i32::MAX`
    /// sentinel set by [`scroll_to_bottom`](Self::scroll_to_bottom) until the
    /// render path clamps them (which only happens when content overflows).
    /// A raw `+ delta` would overflow and panic in debug builds.
    fn scroll_section_by(&mut self, kind: SectionKind, delta: i32) {
        self.user_scrolled_away = true;
        let next = match kind {
            SectionKind::Todo => self.todo_scroll_y.saturating_add(delta).max(0),
            SectionKind::Bash => self.bash_scroll_y.saturating_add(delta).max(0),
            SectionKind::Subagent => self.subagent_scroll_y.saturating_add(delta).max(0),
        };
        self.set_section_scroll(kind, next);
    }

    // ── Drag selection (bash/subagent) ───────────────────────────────
    /// Start a drag selection at screen position `(x, y)`. Returns `true` if
    /// the click landed on a selectable section (bash/subagent); TODO rows
    /// and empty areas cancel any existing selection.
    pub fn begin_selection(&mut self, x: u16, y: u16) -> bool {
        let Some(kind) = self.section_at(y) else {
            self.cancel_selection();
            return false;
        };
        if kind == SectionKind::Todo {
            self.cancel_selection();
            return false;
        }
        let Some(layout) = self.section_layout(kind) else {
            self.cancel_selection();
            return false;
        };
        let scroll = self.section_scroll(kind);
        let content_y = (i32::from(y) - layout.content_top).saturating_add(scroll);
        self.selection_section = Some(kind);
        self.selection_anchor_content_y = content_y;
        self.selection_focus_content_y = content_y;
        self.selection_mouse_down_scroll_y = scroll;
        self.drag_selection = Some((x, y, x, y));
        self.stop_auto_scroll();
        true
    }

    /// Extend the active drag selection to `(x, y)`, clamped to the anchor's
    /// section band so the selection never leaks into a neighbouring section.
    pub fn update_drag_selection(&mut self, x: u16, y: u16) {
        let Some(kind) = self.selection_section else {
            return;
        };
        let Some(layout) = self.section_layout(kind) else {
            return;
        };
        let clamped_y = (i32::from(y)).clamp(layout.top, layout.bottom.saturating_sub(1)) as u16;
        let scroll = self.section_scroll(kind);
        self.selection_focus_content_y =
            (i32::from(clamped_y) - layout.content_top).saturating_add(scroll);
        if let Some((sx, sy, _, _)) = self.drag_selection {
            self.drag_selection = Some((sx, sy, x, clamped_y));
        }
        self.update_auto_scroll(y);
    }

    /// Whether a drag selection is currently active.
    pub fn has_selection(&self) -> bool {
        self.drag_selection.is_some() && self.selection_section.is_some()
    }

    /// Drop the active selection without copying.
    pub fn cancel_selection(&mut self) {
        self.drag_selection = None;
        self.selection_section = None;
        self.selection_anchor_content_y = 0;
        self.selection_focus_content_y = 0;
        self.selection_mouse_down_scroll_y = 0;
        self.stop_auto_scroll();
    }

    /// Extract the text spanned by the active selection (same flow-based
    /// algorithm as the chat). Returns an empty string when there is no
    /// selection or it falls on a non-selectable section.
    pub fn extract_selected_text(&self) -> String {
        let Some(kind) = self.selection_section else {
            return String::new();
        };
        let Some((sx, sy, fx, fy)) = self.drag_selection else {
            return String::new();
        };
        let regions = match kind {
            SectionKind::Bash => &self.bash_text_regions,
            SectionKind::Subagent => &self.subagent_text_regions,
            SectionKind::Todo => return String::new(),
        };
        let Some(layout) = self.section_layout(kind) else {
            return String::new();
        };
        // Convert the screen-space anchor/focus to content rows. The anchor
        // used the scroll at mouse-down, the focus the current scroll, so the
        // selection survives auto-scroll during the drag (like the chat).
        let content_anchor =
            (i32::from(sy) - layout.content_top).saturating_add(self.selection_mouse_down_scroll_y);
        let content_focus =
            (i32::from(fy) - layout.content_top).saturating_add(self.section_scroll(kind));
        let start_content_y = content_anchor.min(content_focus);
        let end_content_y = content_anchor.max(content_focus);
        let (start_x, end_x) = if content_anchor <= content_focus {
            (sx, fx)
        } else {
            (fx, sx)
        };
        extract_text_in_region(regions, start_content_y, end_content_y, start_x, end_x)
    }

    // ── Drag auto-scroll (mirrors the chat) ─────────────────────────
    fn auto_scroll_direction(&self, y: u16) -> i32 {
        let Some(kind) = self.selection_section else {
            return 0;
        };
        let Some(layout) = self.section_layout(kind) else {
            return 0;
        };
        let scroll = self.section_scroll(kind);
        if i32::from(y) <= layout.top + AUTO_SCROLL_THRESHOLD && scroll > 0 {
            return -1;
        }
        if i32::from(y) >= layout.bottom.saturating_sub(AUTO_SCROLL_THRESHOLD) {
            return 1;
        }
        0
    }

    fn auto_scroll_speed_for(&self, y: u16) -> f64 {
        let Some(layout) = self.selection_section.and_then(|k| self.section_layout(k)) else {
            return 0.0;
        };
        let rel = (i32::from(y)).clamp(layout.top, layout.bottom.saturating_sub(1)) - layout.top;
        let h = (layout.bottom - layout.top).max(1);
        let min_dist = rel.min(h - rel);
        if min_dist <= 1 {
            AUTO_SCROLL_SPEED_FAST
        } else if min_dist <= 2 {
            AUTO_SCROLL_SPEED_MEDIUM
        } else {
            AUTO_SCROLL_SPEED_SLOW
        }
    }

    /// Called on every drag: arm auto-scroll when the cursor is near a
    /// section edge, or stop it otherwise.
    pub fn update_auto_scroll(&mut self, y: u16) {
        let dir = self.auto_scroll_direction(y);
        if dir == 0 {
            self.stop_auto_scroll();
        } else if !self.is_auto_scrolling {
            self.is_auto_scrolling = true;
            self.auto_scroll_accumulator = 0.0;
            self.auto_scroll_speed = self.auto_scroll_speed_for(y);
        }
    }

    /// Advance drag auto-scroll by `delta_time` (called once per render
    /// frame). Clamps the section scroll and stops at the content bounds.
    pub fn handle_auto_scroll(&mut self, delta_time: f64) {
        if !self.is_auto_scrolling {
            return;
        }
        let Some(kind) = self.selection_section else {
            self.stop_auto_scroll();
            return;
        };
        let dir = if let Some((_, _, _, fy)) = self.drag_selection {
            self.auto_scroll_direction(fy)
        } else {
            0
        };
        if dir == 0 {
            self.stop_auto_scroll();
            return;
        }
        self.auto_scroll_accumulator += self.auto_scroll_speed * delta_time * f64::from(dir);
        let int_scroll = self.auto_scroll_accumulator.trunc() as i32;
        if int_scroll == 0 {
            return;
        }
        self.auto_scroll_accumulator -= int_scroll as f64;
        let max_scroll = self.section_max_scroll(kind);
        let cur = self.section_scroll(kind);
        // Saturating: `cur` may transiently hold the `i32::MAX` scroll-to-
        // bottom sentinel before the render clamps it; a raw `+` would
        // overflow when auto-scrolling downward.
        let next = (cur.saturating_add(int_scroll)).clamp(0, max_scroll);
        if next == cur {
            self.stop_auto_scroll();
            return;
        }
        self.set_section_scroll(kind, next);
        // Keep the focus content row tracking the moved content.
        self.selection_focus_content_y = self.selection_focus_content_y.saturating_add(int_scroll);
    }

    pub fn stop_auto_scroll(&mut self) {
        if self.is_auto_scrolling {
            self.is_auto_scrolling = false;
            self.auto_scroll_accumulator = 0.0;
            self.auto_scroll_speed = 0.0;
        }
    }

    /// Maximum scroll offset of a section: total content rows minus the
    /// visible inner height. `inner_h = band_height - 3` (TOP_GAP + TOP_PAD +
    /// BOTTOM_PAD, identical for every section).
    pub(crate) fn section_max_scroll(&mut self, kind: SectionKind) -> i32 {
        let Some(layout) = self.section_layout(kind).copied() else {
            return 0;
        };
        let inner_h = (layout.bottom - layout.top).saturating_sub(3).max(1);
        let total = match kind {
            SectionKind::Todo => self.todos.len() as i32,
            SectionKind::Bash => self.bash_buffer().len() as i32,
            SectionKind::Subagent => {
                let wrap_w = self.text_regions_w.max(1);
                self.subagent_section_rows(wrap_w)
                    .iter()
                    .map(|&r| i32::from(r))
                    .sum()
            }
        };
        (total - inner_h).max(0)
    }

    /// Scroll sections to bottom (auto-scroll on new output).
    pub fn scroll_to_bottom(&mut self) {
        self.user_scrolled_away = false;
        self.bash_scroll_y = i32::MAX;
        self.subagent_scroll_y = i32::MAX;
        self.todo_scroll_y = i32::MAX;
    }

    /// Reset all per-section scrolls to top.
    pub fn reset_scroll(&mut self) {
        self.user_scrolled_away = false;
        self.bash_scroll_y = 0;
        self.subagent_scroll_y = 0;
        self.todo_scroll_y = 0;
        self.scroll_y = 0;
        self.cancel_selection();
    }

    /// Set the viewport height (called from render). The per-section scrolls
    /// are clamped during render by each section's render function.
    pub fn set_visible_height(&mut self, h: i32) {
        self.visible_height = h;
    }

    /// Whether the user has manually scrolled away from the bottom.
    pub fn is_scrolled_up(&self) -> bool {
        self.user_scrolled_away
    }

    /// Set the current todos, replacing any existing ones.
    pub fn set_todos(&mut self, todos: Vec<TodoItem>) {
        self.todos = todos;
        self.mark_activity(SectionKind::Todo);
    }

    /// Start a new PTY session for a bash command.
    pub fn start_pty(&mut self, command: String, workdir: Option<String>) {
        self.next_pty_id += 1;
        let id = format!("pty-{}", self.next_pty_id);
        let kind = if command.starts_with("subagent:") {
            SectionKind::Subagent
        } else {
            SectionKind::Bash
        };
        self.mark_activity(kind);
        self.pty_sessions.push(PtySession {
            id,
            command,
            output: String::new(),
            workdir,
            status: PtyStatus::Running,
        });
        // Bound memory: evict the oldest COMPLETED sessions beyond the cap.
        // A running session is never evicted (output updates target the last
        // running one), but the agent runs commands sequentially, so the oldest
        // entries are always finished.
        self.pty_gen = self.pty_gen.wrapping_add(1);
        while self.pty_sessions.len() > MAX_PTY_SESSIONS
            && !matches!(
                self.pty_sessions.first(),
                Some(p) if matches!(p.status, PtyStatus::Running)
            )
        {
            self.pty_sessions.remove(0);
        }
    }

    /// Append output for the last running PTY session.
    pub fn update_last_pty(&mut self, output: String) {
        let mut found_kind = None;
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            found_kind = if session.command.starts_with("subagent:") {
                Some(SectionKind::Subagent)
            } else {
                Some(SectionKind::Bash)
            };
            session.output.push_str(&output);
            Self::truncate_output(&mut session.output);
            self.pty_gen = self.pty_gen.wrapping_add(1);
        }
        if let Some(kind) = found_kind {
            self.mark_activity(kind);
        }
    }

    /// Mark the last running PTY session as completed.
    pub fn complete_last_pty(&mut self, final_output: String) {
        let mut final_output = final_output;
        Self::truncate_output(&mut final_output);
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            session.output = final_output;
            session.status = PtyStatus::Completed;
            self.pty_gen = self.pty_gen.wrapping_add(1);
        }
    }

    /// Mark the last running PTY session as failed.
    pub fn fail_last_pty(&mut self, error: String) {
        let mut error = error;
        Self::truncate_output(&mut error);
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            session.output = error;
            session.status = PtyStatus::Failed;
            self.pty_gen = self.pty_gen.wrapping_add(1);
        }
    }

    /// Trim a PTY output string to `MAX_PTY_OUTPUT_CHARS`, keeping the TAIL
    /// (the panel shows the most recent output). Cuts at a line boundary when
    /// possible so a partial line never appears at the top of the buffer.
    fn truncate_output(output: &mut String) {
        if output.len() <= MAX_PTY_OUTPUT_CHARS {
            return;
        }
        let start = output.len() - MAX_PTY_OUTPUT_CHARS;
        let cut = output[start..].find('\n').map_or(start, |i| start + i + 1);
        output.drain(..cut);
    }

    /// Derived line buffer of all bash PTY sessions (`$ command` + output lines)
    /// in order. Cached until the next PTY mutation: rebuilding the entire
    /// accumulated output on every frame was the dominant per-frame cost of the
    /// right panel as a session grew long.
    pub(crate) fn bash_buffer(&mut self) -> &[String] {
        if self.pty_gen != self.bash_cache_gen {
            self.bash_buffer_cache.clear();
            for pty in &self.pty_sessions {
                if pty.command.starts_with("subagent:") {
                    continue;
                }
                // Header: $ command (simulating a shell prompt)
                self.bash_buffer_cache.push(format!("$ {}", pty.command));
                for line in pty.output.lines() {
                    self.bash_buffer_cache.push(line.to_string());
                }
            }
            self.bash_cache_gen = self.pty_gen;
        }
        &self.bash_buffer_cache
    }

    /// Derived line buffer of all subagent PTY sessions (command header + output
    /// lines) in order. Same-agent entries are deduplicated by `app.rs`.
    /// Cached until the next PTY mutation (see [`Self::bash_buffer`]).
    pub(crate) fn subagent_buffer(&mut self) -> &[String] {
        if self.pty_gen != self.subagent_cache_gen {
            self.subagent_buffer_cache.clear();
            for pty in &self.pty_sessions {
                if !pty.command.starts_with("subagent:") {
                    continue;
                }
                // Header: the command line (e.g., "subagent: opencode")
                self.subagent_buffer_cache.push(pty.command.clone());
                for line in pty.output.lines() {
                    self.subagent_buffer_cache.push(line.to_string());
                }
            }
            self.subagent_cache_gen = self.pty_gen;
        }
        &self.subagent_buffer_cache
    }

    /// Total content rows (command header + optional input line + markdown
    /// body) per subagent session, cached against `subagent_layout_gen` and
    /// `wrap_w` so the markdown parse only runs when needed. During
    /// streaming, rebuilds are coalesced to at most one per
    /// [`SUBAGENT_REBUILD_INTERVAL`] — when throttled, the previous rows are
    /// served (and `subagent_layout_gen` stays behind `pty_gen`, so the
    /// body cache keeps agreeing with them). A wrap-width change always
    /// rebuilds immediately. Mirrors the render exactly: the body width here
    /// is the same `wrap_w` the section renderer passes to
    /// [`MarkdownRenderable`](cosh_tui::core::renderables::markdown::MarkdownRenderable).
    pub(crate) fn subagent_section_rows(&mut self, wrap_w: u16) -> &[u16] {
        let width_changed = wrap_w != self.subagent_rows_cache_w;
        let gen_changed = self.pty_gen != self.subagent_layout_gen;
        let rebuild_due = width_changed
            || (gen_changed
                && self.last_subagent_rebuild.elapsed() >= self.subagent_rebuild_interval);
        if rebuild_due {
            if !width_changed {
                self.last_subagent_rebuild = Instant::now();
            }
            self.subagent_layout_gen = self.pty_gen;
            self.subagent_rows_cache.clear();
            for pty in &self.pty_sessions {
                if !pty.command.starts_with("subagent:") {
                    continue;
                }
                let (input, body) = split_subagent_output(&pty.output);
                let mut rows: u16 = 1; // command header
                if input.is_some() {
                    rows += 1;
                }
                let clean = sanitize_subagent_text(body);
                if !clean.trim().is_empty() {
                    rows = rows.saturating_add(estimate_height(&clean, wrap_w));
                }
                self.subagent_rows_cache.push(rows);
            }
            self.subagent_rows_cache_w = wrap_w;
        }
        &self.subagent_rows_cache
    }

    /// Check if there's any content to show in the panel.
    pub fn has_content(&self) -> bool {
        !self.todos.is_empty() || !self.pty_sessions.is_empty()
    }
}

impl Default for RightPanelState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_last_pty_appends_output() {
        let mut state = RightPanelState::new();
        state.start_pty("echo hello".to_string(), None);

        state.update_last_pty("hello".to_string());
        assert_eq!(state.pty_sessions[0].output, "hello");

        state.update_last_pty(" world".to_string());
        assert_eq!(
            state.pty_sessions[0].output, "hello world",
            "expected append, got: {:?}",
            state.pty_sessions[0].output,
        );
    }

    #[test]
    fn update_last_pty_only_updates_running() {
        let mut state = RightPanelState::new();
        state.start_pty("cmd1".to_string(), None);
        state.complete_last_pty("done".to_string());

        state.start_pty("cmd2".to_string(), None);

        state.update_last_pty("output2".to_string());

        assert_eq!(state.pty_sessions[0].output, "done");
        assert_eq!(state.pty_sessions[1].output, "output2");
    }

    #[test]
    fn complete_last_pty_marks_completed() {
        let mut state = RightPanelState::new();
        state.start_pty("echo hi".to_string(), None);

        state.complete_last_pty("final output".to_string());
        assert_eq!(state.pty_sessions[0].status, PtyStatus::Completed);
    }

    #[test]
    fn fail_last_pty_marks_failed() {
        let mut state = RightPanelState::new();
        state.start_pty("bad cmd".to_string(), None);

        state.fail_last_pty("error!".to_string());
        assert_eq!(state.pty_sessions[0].status, PtyStatus::Failed);
        assert_eq!(state.pty_sessions[0].output, "error!");
    }

    /// Reproducer: `scroll_to_bottom()` stores `i32::MAX` as the scroll
    /// offset sentinel. When the panel content fits the viewport, the render
    /// path never clamps that value (the clamp only runs when `has_scroll`
    /// is true). A later `scroll_down()` then evaluates `i32::MAX + delta`,
    /// which overflows and panics in debug builds. This happens right after
    /// the terminal is resized large enough for the right panel to appear
    /// and the user scrolls inside it.
    #[test]
    fn scroll_down_after_scroll_to_bottom_does_not_overflow() {
        let mut state = RightPanelState::new();
        state.start_pty("echo hi".to_string(), None);
        state.push_section_layout(SectionKind::Bash, 0, 30);
        state.scroll_to_bottom();

        // Reproduce the post-resize scroll: offset is still i32::MAX because
        // the content fits the viewport, so no render clamp has run.
        // Scrolling down while pinned to the bottom must be a no-op
        // (saturating), never an overflow panic.
        state.scroll_down_at(5, 3);
        assert_eq!(state.bash_scroll_y, i32::MAX);

        state.scroll_down_at(5, 20);
        assert_eq!(state.bash_scroll_y, i32::MAX);

        // And scrolling up from the bottom sentinel must work normally.
        state.scroll_up_at(5, 5);
        assert_eq!(state.bash_scroll_y, i32::MAX - 5);
    }

    /// Same overflow applies to the subagent and todo sections — the
    /// `i32::MAX` sentinel from `scroll_to_bottom()` survives the render
    /// when content fits, so scrolling must saturate on those branches too.
    #[test]
    fn scroll_down_after_scroll_to_bottom_saturates_all_sections() {
        // Subagent branch: only subagent PTYs present.
        let mut sub = RightPanelState::new();
        sub.start_pty("subagent: opencode".to_string(), None);
        sub.push_section_layout(SectionKind::Subagent, 0, 30);
        sub.scroll_to_bottom();
        sub.scroll_down_at(5, 3);
        assert_eq!(sub.subagent_scroll_y, i32::MAX);
        sub.scroll_up_at(5, 5);
        assert_eq!(sub.subagent_scroll_y, i32::MAX - 5);

        // Todo branch: only todos present, no PTYs.
        let mut todos = RightPanelState::new();
        todos.set_todos(vec![TodoItem {
            status: "pending".to_string(),
            content: "do the thing".to_string(),
        }]);
        todos.push_section_layout(SectionKind::Todo, 0, 30);
        todos.scroll_to_bottom();
        todos.scroll_down_at(5, 3);
        assert_eq!(todos.todo_scroll_y, i32::MAX);
        todos.scroll_up_at(5, 5);
        assert_eq!(todos.todo_scroll_y, i32::MAX - 5);
    }

    /// The right panel must not accumulate every bash command of the process
    /// lifetime — the oldest COMPLETED sessions are evicted beyond the cap.
    #[test]
    fn pty_sessions_are_evicted_beyond_cap() {
        let mut state = RightPanelState::new();
        for i in 0..(MAX_PTY_SESSIONS + 10) {
            state.start_pty(format!("cmd{i}"), None);
            state.complete_last_pty(format!("out{i}"));
        }
        assert!(state.pty_sessions.len() <= MAX_PTY_SESSIONS);
        // The most recent commands survive; the oldest are gone.
        assert!(
            state
                .pty_sessions
                .iter()
                .any(|p| p.command == format!("cmd{}", MAX_PTY_SESSIONS + 9))
        );
        assert!(!state.pty_sessions.iter().any(|p| p.command == "cmd0"));
    }

    /// A running session is never evicted (output updates target the last
    /// running session), even when the cap is exceeded.
    #[test]
    fn running_pty_is_never_evicted() {
        let mut state = RightPanelState::new();
        for i in 0..(MAX_PTY_SESSIONS + 5) {
            state.start_pty(format!("done{i}"), None);
            state.complete_last_pty("ok".to_string());
        }
        // Leave the newest session running while over the cap.
        state.start_pty("running".to_string(), None);
        assert!(state.pty_sessions.iter().any(|p| p.command == "running"));
    }

    /// Output is truncated to `MAX_PTY_OUTPUT_CHARS`, keeping the TAIL (the
    /// panel displays the most recent output).
    #[test]
    fn pty_output_is_truncated_to_max_chars() {
        let mut state = RightPanelState::new();
        state.start_pty("cmd".to_string(), None);
        let big = "x".repeat(MAX_PTY_OUTPUT_CHARS + 5000);
        state.update_last_pty(big);
        let session = state.pty_sessions.last().unwrap();
        assert!(session.output.len() <= MAX_PTY_OUTPUT_CHARS);
        // The tail is preserved.
        assert!(session.output.ends_with(&"x".repeat(100)));
    }

    /// The derived bash/subagent buffers are cached and only rebuilt when a
    /// PTY mutation happens (previously rebuilt from scratch every frame).
    #[test]
    fn pty_buffers_rebuild_only_on_mutation() {
        let mut state = RightPanelState::new();
        state.start_pty("echo hi".to_string(), None);
        state.complete_last_pty("hi".to_string());

        let first = state.bash_buffer().to_vec();
        assert_eq!(first, vec!["$ echo hi".to_string(), "hi".to_string()]);

        // No mutation → the cached slice is served unchanged.
        assert_eq!(state.bash_buffer(), first);

        // Mutation bumps the generation → the cache is rebuilt with new output.
        state.start_pty("echo yo".to_string(), None);
        state.complete_last_pty("yo".to_string());
        assert_eq!(
            state.bash_buffer(),
            vec![
                "$ echo hi".to_string(),
                "hi".to_string(),
                "$ echo yo".to_string(),
                "yo".to_string(),
            ]
        );
    }

    /// Bash and subagent sessions are separated into their own buffers.
    #[test]
    fn subagent_and_bash_buffers_are_separate() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.complete_last_pty("agent output".to_string());
        state.start_pty("echo hi".to_string(), None);
        state.complete_last_pty("hi".to_string());

        assert_eq!(state.bash_buffer().len(), 2);
        assert_eq!(
            state.subagent_buffer().to_vec(),
            vec!["subagent: opencode".to_string(), "agent output".to_string()]
        );
    }

    /// The first "→ cosh:" line of a subagent output is the main agent's
    /// input; everything after it is the subagent body. Outputs without the
    /// prefix (e.g. after tail truncation) are all body.
    #[test]
    fn split_subagent_output_extracts_input_line() {
        assert_eq!(
            split_subagent_output("plain body\n"),
            (None, "plain body\n")
        );
        assert_eq!(
            split_subagent_output("→ cosh: review this\nbody\n"),
            (Some("→ cosh: review this"), "body\n")
        );
        assert_eq!(
            split_subagent_output("→ cosh: hi"),
            (Some("→ cosh: hi"), "")
        );
    }

    /// The subagent section rows add up the command header, the optional
    /// input line and the markdown body rows, so the box height matches the
    /// laid-out markdown (never clips the report).
    #[test]
    fn subagent_rows_reflect_markdown_height() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(
            "→ cosh: review this\n# Title\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n"
                .to_string(),
        );

        let w = 20u16;
        let rows = state.subagent_section_rows(w);
        assert_eq!(rows.len(), 1, "one subagent session");
        let (input, body) = split_subagent_output(
            "→ cosh: review this\n# Title\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n",
        );
        assert_eq!(input, Some("→ cosh: review this"));
        let expected = 1 + 1 + estimate_height(&sanitize_subagent_text(body), w);
        assert_eq!(
            rows[0], expected,
            "header + input line + markdown body rows"
        );
        // A code block is taller than its raw line count (padding rows).
        assert!(u32::from(rows[0]) > body.lines().count() as u32 + 2);
    }

    /// The height cache is rebuilt only when output (`pty_gen`) or the wrap
    /// width changes; a narrower box wraps more and yields more rows.
    #[test]
    fn subagent_rows_cache_invalidates_on_width_and_gen_change() {
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = Duration::ZERO; // no throttle in tests
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(
            "aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee ffffffffff\n".to_string(),
        );

        let narrow = state.subagent_section_rows(10)[0];
        // Same gen + same width → cached slice, no rebuild.
        assert_eq!(state.subagent_section_rows(10)[0], narrow);
        // Wider box → fewer rows.
        let wide = state.subagent_section_rows(60)[0];
        assert!(
            narrow > wide,
            "{narrow} rows at w=10 should exceed {wide} at w=60"
        );
        // New output → rebuilt at the current width.
        state.update_last_pty("more output that wraps\n".to_string());
        let grown = state.subagent_section_rows(60)[0];
        assert!(grown > wide, "growing output must increase the cached rows");
    }

    /// A session with no output yet still occupies one row (the command
    /// header) so the section is visible while the agent runs.
    #[test]
    fn subagent_rows_header_only_for_empty_output() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        assert_eq!(state.subagent_section_rows(30), &[1]);
    }

    /// During streaming, layout rebuilds are coalesced to at most one per
    /// `subagent_rebuild_interval`: a gen change within the interval serves
    /// the previous (stale) rows and keeps `subagent_layout_gen` behind
    /// `pty_gen`, so heights and bodies always agree on the same generation.
    #[test]
    fn subagent_rebuild_is_throttled_by_interval() {
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = Duration::from_secs(60);
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty("short body\n".to_string());

        let rows = state.subagent_section_rows(30).to_vec();
        let layout_gen_snapshot = state.subagent_layout_gen;
        assert_eq!(rows, vec![2], "header + 1 body row");

        // New output arrives within the interval: no rebuild (stale rows).
        state.update_last_pty(
            "a much longer line that wraps around and needs more rows to display\n".to_string(),
        );
        assert_eq!(state.subagent_section_rows(30), rows);
        assert_eq!(state.subagent_layout_gen, layout_gen_snapshot);

        // Once the interval passes, the rebuild happens and picks the change.
        state.last_subagent_rebuild = Instant::now()
            .checked_sub(Duration::from_secs(61))
            .unwrap_or_else(Instant::now);
        let rebuilt = state.subagent_section_rows(30).to_vec();
        assert_ne!(rebuilt, rows, "rebuild must pick up the new output");
        assert_eq!(state.subagent_layout_gen, state.pty_gen);
    }

    // ── Section layout / cursor-targeted scroll ────────────────────

    /// The cursor's screen row resolves to the owning section band; gaps and
    /// rows outside any band resolve to `None`.
    #[test]
    fn section_at_resolves_by_screen_row() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Bash, 2, 12);
        state.push_section_layout(SectionKind::Subagent, 13, 30);

        assert_eq!(state.section_at(2), Some(SectionKind::Bash));
        assert_eq!(state.section_at(11), Some(SectionKind::Bash));
        assert_eq!(state.section_at(12), None, "band end is exclusive");
        assert_eq!(state.section_at(15), Some(SectionKind::Subagent));
        assert_eq!(state.section_at(31), None);
    }

    /// `scroll_up_at`/`scroll_down_at` scroll ONLY the section under the row;
    /// rows in a gap or outside the panel leave every offset untouched.
    #[test]
    fn scroll_at_targets_only_the_section_under_the_cursor() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Bash, 0, 10);
        state.push_section_layout(SectionKind::Subagent, 11, 30);
        state.bash_scroll_y = 0;
        state.subagent_scroll_y = 0;

        state.scroll_down_at(3, 5);
        assert_eq!(state.bash_scroll_y, 5);
        assert_eq!(state.subagent_scroll_y, 0, "other section untouched");

        state.scroll_down_at(20, 5);
        assert_eq!(state.bash_scroll_y, 5, "other section untouched");
        assert_eq!(state.subagent_scroll_y, 5);

        state.scroll_up_at(20, 2);
        assert_eq!(state.bash_scroll_y, 5, "other section untouched");
        assert_eq!(state.subagent_scroll_y, 3);

        // A row inside the gap scrolls nothing.
        state.scroll_down_at(10, 9);
        assert_eq!(state.bash_scroll_y, 5);
        assert_eq!(state.subagent_scroll_y, 3);
    }

    // ── Drag selection ─────────────────────────────────────────────

    /// A click on a bash/subagent content row starts a selection; a click on
    /// a TODO row or a gap cancels any active selection.
    #[test]
    fn begin_selection_accepts_bash_and_subagent_only() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Todo, 0, 10);
        state.push_section_layout(SectionKind::Bash, 11, 20);
        state.push_section_layout(SectionKind::Subagent, 21, 30);

        assert!(state.begin_selection(5, 14));
        assert_eq!(state.selection_section, Some(SectionKind::Bash));
        assert_eq!(
            state.selection_anchor_content_y, 1,
            "row 14 maps to bash content row 1 (content_top = 13)"
        );
        assert!(state.has_selection());

        assert!(state.begin_selection(5, 25));
        assert_eq!(state.selection_section, Some(SectionKind::Subagent));

        // TODO rows never select; the old selection is dropped.
        assert!(!state.begin_selection(5, 3));
        assert!(!state.has_selection());
        assert!(state.selection_section.is_none());

        // Rows outside any band cancel too.
        assert!(!state.begin_selection(5, 50));
    }

    /// Dragging clamps the focus to the anchor's section band, so a drag
    /// that spills into the neighbouring section does not leak rows.
    #[test]
    fn drag_selection_is_clamped_to_the_anchor_section() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Bash, 0, 10);
        state.push_section_layout(SectionKind::Subagent, 11, 30);
        state.bash_scroll_y = 0;

        state.begin_selection(3, 4); // bash band, content row 2
        state.update_drag_selection(9, 25); // drag far below the band
        let (_, _, _, fy) = state.drag_selection.unwrap();
        assert_eq!(fy, 9, "focus clamped to the bash band bottom");
        assert_eq!(state.selection_focus_content_y, 7);

        // Dragging back to the top clamps to the band top; the content row
        // goes negative (row 0 sits above the content area's first row).
        state.update_drag_selection(9, 0);
        let (_, _, _, fy) = state.drag_selection.unwrap();
        assert_eq!(fy, 0);
        assert_eq!(state.selection_focus_content_y, -2);
    }

    /// `extract_selected_text` walks the flow-based algorithm over the
    /// section's text regions and returns the spanned text, joining rows.
    #[test]
    fn extract_selected_text_slices_the_region_rows() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Bash, 0, 10);
        state.bash_scroll_y = 0;
        state.bash_text_regions = vec![
            TextRegion {
                y1: 0,
                y2: 1,
                x1: 0,
                x2: 8,
                text: "alpha".to_string(),
            },
            TextRegion {
                y1: 1,
                y2: 2,
                x1: 0,
                x2: 8,
                text: "bravo".to_string(),
            },
        ];

        state.begin_selection(1, 3); // content row 1 (content_top = 2), x=1
        state.update_drag_selection(5, 4); // content row 2, x=5
        let text = state.extract_selected_text();
        assert_eq!(text, "ravo");

        // Single-row selection keeps both x bounds on that row.
        state.begin_selection(1, 3);
        state.update_drag_selection(3, 3);
        assert_eq!(state.extract_selected_text(), "ra");

        // A click on a TODO row cancels the selection → empty text.
        state.push_section_layout(SectionKind::Todo, 20, 30);
        state.begin_selection(2, 22);
        assert_eq!(state.extract_selected_text(), "");
    }

    /// Selection content rows use the scroll at mouse-down; auto-scroll
    /// during the drag keeps the extracted rows tracking the moved content.
    #[test]
    fn extract_selected_text_tracks_content_during_auto_scroll() {
        let mut state = RightPanelState::new();
        state.push_section_layout(SectionKind::Bash, 0, 10);
        state.bash_scroll_y = 0;
        state.bash_text_regions = (0..20)
            .map(|i| TextRegion {
                y1: i,
                y2: i + 1,
                x1: 0,
                x2: 8,
                text: format!("line{i}"),
            })
            .collect();

        state.begin_selection(0, 2); // content row 0
        state.update_drag_selection(0, 4); // content row 2
        // Content scrolls down by 2 while dragging (mouse near the edge).
        state.set_section_scroll(SectionKind::Bash, 2);
        state.selection_focus_content_y += 2;
        let text = state.extract_selected_text();
        assert_eq!(text, "line0\nline1\nline2\nline3");
    }
}
