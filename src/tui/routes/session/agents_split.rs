//! Split-view "panes" for the future `/agents` route: one pane per agent
//! CLI (index 0 = the cosh daemon itself), laid out over the session area
//! with a simple bin-packing heuristic.
//!
//! This module owns ONLY the pane model + layout + painting. The prompt is
//! a single shared input that lives outside this area (`compute_prompt_area`
//! in `app/render.rs`), so dispatching a draft to a pane is not this
//! module's concern — it only keeps the per-pane drafts so the future
//! dispatcher can read the focused pane's target.
//!
//! Layout heuristic (deliberately approximate — "fits nicely", not a perfect
//! bin-packing): the daemon pane is a full-width horizontal strip on top
//! (its height shrinks as the pane count grows), the remaining agents fill
//! the rest as equal columns when each column stays readable (>= 24 cols),
//! otherwise as two balanced rows (ceil/floor split, the top row slightly
//! taller when the height is odd). The last row may end up narrower/taller
//! than the others; what matters is that the panes tile the area with only
//! 1-cell gaps and no holes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;
use cosh_tools::subagent::events::{PlanEntryStatus, SubagentEvent, ToolCallStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::routes::session::right_panel::subagent_theme_key;
use crate::routes::session::right_panel::types::{
    activity_lines, SubagentActivity, SubagentActivityLine, SubagentBlockRenderer,
};
use crate::theme::{Theme, blend, contrast_fg, rgba_color};
use crate::util::text_region::{TextRegion, extract_text_in_region, text_from_cell_row};

/// Display label of the daemon pane (agent index 0): the TUI's own agent.
pub const DAEMON_LABEL: &str = "cosh";

/// Hotkey of the daemon pane. The remaining panes take `1`..=`9` in picker
/// order, so the daemon is always reachable with a single keypress.
pub const DAEMON_HOTKEY: char = '0';

/// Minimum readable width of one agent column before the layout falls back
/// to a second row (see the module-level heuristic description).
const MIN_COLUMN_WIDTH: u16 = 24;

/// Clamp bounds of the daemon strip's height.
const DAEMON_MIN_HEIGHT: u16 = 3;
const DAEMON_MAX_HEIGHT: u16 = 8;

/// Gap cells (1 column / 1 row) between neighbouring panes.
const GAP: u16 = 1;

/// Hotkey label of pane `index`: `'0'` for the daemon, `'1'..='9'` for the
/// agents that follow. `None` past the nine numbered panes.
#[must_use]
pub fn pane_hotkey(index: usize) -> Option<char> {
    match index {
        0 => Some(DAEMON_HOTKEY),
        1..=9 => Some((b'0' + index as u8) as char),
        _ => None,
    }
}

/// One agent pane: identity, live activity stream and lifecycle state.
#[derive(Debug, Clone, Default)]
pub struct AgentPane {
    /// Agent CLI name (`"claude"`, `"kilo"`, …). The daemon pane is `"cosh"`.
    pub agent: String,
    /// Structured live transcript, REUSED from the right panel's
    /// `SubagentActivity` so Message/Thought chunks coalesce and tool-call
    /// patches apply exactly like the existing sub-agent box.
    pub activity: SubagentActivity,
    /// Whether a sub-agent turn is currently running in this pane.
    pub running: bool,
    /// Cooperative stop flag handed to the future dispatch layer.
    pub stop_signal: Option<Arc<AtomicBool>>,
    /// Session id for a future `resume` flow.
    pub session_id: Option<String>,
    /// Top of the visible window in content rows — the chat's `scroll_y`.
    /// Re-clamped every render against the real content; sticky-bottom
    /// follow is [`AgentPane::auto_follow`].
    pub scroll_y: u16,
    /// The chat's sticky-bottom contract: new content drags the view along
    /// until the user scrolls away; scrolling back to the bottom re-engages
    /// it. A new turn re-engages it unconditionally.
    pub auto_follow: bool,
    /// Scroll offset the LAST render actually used and the measured content/
    /// body heights: the keyboard/wheel paths clamp against them without
    /// re-laying-out, and the selection math converts screen rows to content
    /// rows with the effective offset.
    pub effective_scroll: u16,
    pub content_height: u16,
    pub body_height: u16,
}

impl AgentPane {
    #[must_use]
    pub fn new(agent: String) -> Self {
        Self {
            agent,
            activity: SubagentActivity::default(),
            running: false,
            stop_signal: None,
            session_id: None,
            scroll_y: 0,
            auto_follow: true,
            effective_scroll: 0,
            content_height: 0,
            body_height: 0,
        }
    }

    /// Scroll the pane by `rows` (negative = up) against the LAST measured
    /// heights. Scrolling anywhere above the bottom un-pins follow; reaching
    /// the bottom re-engages it (the chat's `sync_manual_scroll_state`).
    /// Returns whether the offset moved.
    pub fn scroll_by(&mut self, rows: i32) -> bool {
        let max = self.content_height.saturating_sub(self.body_height);
        let next = (i32::from(self.scroll_y) + rows).clamp(0, i32::from(max));
        if next == i32::from(self.scroll_y) {
            return false;
        }
        self.scroll_y = u16::try_from(next).unwrap_or(max);
        self.auto_follow = self.scroll_y >= max;
        true
    }

    /// Jump to the oldest content (un-pins follow).
    pub fn scroll_to_top(&mut self) {
        self.scroll_y = 0;
        self.auto_follow = false;
    }

    /// Jump to the newest content (re-engages follow).
    pub fn scroll_to_bottom(&mut self) {
        self.auto_follow = true;
    }

    /// Content row under screen row `y` of this pane's body (body starts one
    /// row below the pane top), using the LAST render's effective offset.
    pub fn content_row_at(&self, rect: Rect, y: u16) -> i32 {
        let body_top = i32::from(rect.y + 1);
        let row = i32::from(y).saturating_sub(body_top);
        row.saturating_add(i32::from(self.effective_scroll))
    }
}

/// State of the whole `/agents` split view.
#[derive(Debug, Default)]
pub struct AgentsSplitState {
    /// One pane per agent. Index 0 is ALWAYS the cosh daemon pane; the rest
    /// follow the picker's chosen order.
    pub panes: Vec<AgentPane>,
    /// Focused pane index (default: the daemon).
    pub focused: usize,
    /// Screen rects of the LAST rendered frame — the mouse dispatcher
    /// hit-tests these, exactly like `SessionView::pill_area`.
    pub pane_rects: Vec<Rect>,
    /// Per-pane draft text, so the future dispatcher knows which pane a
    /// submission targets. Starts simple; scrolling/history live elsewhere.
    pub drafts: Vec<String>,
    /// Persistent chat-style markdown renderers for the panes' Message/
    /// Thought blocks — the SAME cache pattern the right panel's sub-agent
    /// window uses (`SubagentBlockRenderer` survives across frames so
    /// `set_content` re-parses only the streamed tail). Key:
    /// `{pane_index}:{timeline_index}`. Lives on the split state (not the
    /// pane) so the pane model stays plain data.
    pub block_cache: HashMap<String, SubagentBlockRenderer>,
    /// Copy-by-selection state (the chat's drag contract, one selection for
    /// the whole split): the pane it started in, the screen-space anchor/
    /// focus, and the content rows they map to.
    pub selection_pane: Option<usize>,
    pub drag_selection: Option<(u16, u16, u16, u16)>,
    pub selection_anchor_content_y: i32,
    pub selection_focus_content_y: i32,
    /// Scroll the anchor was taken with — the extraction pairs it with the
    /// CURRENT focus scroll so the selection survives scrolling mid-drag.
    pub selection_mouse_down_scroll: u16,
    /// Text regions of the LAST rendered frame, per pane (content-row
    /// labeled) — the extraction source, rebuilt every frame like the right
    /// panel's regions so it always matches what is painted.
    pub pane_regions: Vec<Vec<TextRegion>>,
}

impl AgentsSplitState {
    /// Build the pane set. The daemon (`cosh`) is inserted at index 0 unless
    /// the caller already put it first; `drafts` stays aligned with `panes`.
    #[must_use]
    pub fn new(agents: Vec<String>) -> Self {
        let mut panes: Vec<AgentPane> = agents
            .into_iter()
            .filter(|a| !a.is_empty())
            .map(AgentPane::new)
            .collect();
        if panes.first().map(|p| p.agent.as_str()) != Some(DAEMON_LABEL) {
            panes.insert(0, AgentPane::new(DAEMON_LABEL.to_string()));
        }
        let drafts = vec![String::new(); panes.len()];
        Self {
            panes,
            focused: 0,
            pane_rects: Vec::new(),
            drafts,
            block_cache: HashMap::new(),
            selection_pane: None,
            drag_selection: None,
            selection_anchor_content_y: 0,
            selection_focus_content_y: 0,
            selection_mouse_down_scroll: 0,
            pane_regions: Vec::new(),
        }
    }

    /// Number of panes (daemon included).
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Move focus to the next pane, wrapping around.
    pub fn focus_next(&mut self) {
        if !self.panes.is_empty() {
            self.focused = (self.focused + 1) % self.panes.len();
        }
    }

    /// Move focus to the previous pane, wrapping around.
    pub fn focus_prev(&mut self) {
        if !self.panes.is_empty() {
            self.focused = (self.focused + self.panes.len() - 1) % self.panes.len();
        }
    }

    /// Focus the pane bound to hotkey `ch` (`'0'` = daemon, `'1'..='9'` =
    /// the agents in picker order). Returns whether a pane matched.
    pub fn focus_hotkey(&mut self, ch: char) -> bool {
        let index = match ch {
            c if c == DAEMON_HOTKEY => 0,
            '1'..='9' => usize::from(ch as u8 - b'0'),
            _ => return false,
        };
        if index < self.panes.len() {
            self.focused = index;
            true
        } else {
            false
        }
    }

    /// The currently focused pane, if any.
    #[must_use]
    pub fn focused_pane(&self) -> Option<&AgentPane> {
        self.panes.get(self.focused)
    }

    /// Scroll the FOCUSED pane by `rows` (negative = up), the keyboard
    /// scroll path. Mirrors the chat's `scroll_by_raw` + accumulator reset
    /// cadence: each keypress moves whole rows against the LAST measured
    /// heights, no acceleration.
    pub fn scroll_focused_by(&mut self, rows: i32) {
        if let Some(pane) = self.panes.get_mut(self.focused) {
            pane.scroll_by(rows);
        }
    }

    /// Scroll the pane whose rect contains screen row `y` (the wheel path,
    /// the right panel's `scroll_up_at` contract: the pane under the pointer
    /// moves, the others stay put). `false` when no pane owns the row.
    pub fn scroll_at(&mut self, y: u16, rows: i32) -> bool {
        let Some(index) = self.pane_rects.iter().position(|r| y >= r.y && y < r.bottom()) else {
            return false;
        };
        let Some(pane) = self.panes.get_mut(index) else {
            return false;
        };
        pane.scroll_by(rows)
    }

    /// Jump the focused pane to the oldest / newest content.
    pub fn scroll_focused_to_top(&mut self) {
        if let Some(pane) = self.panes.get_mut(self.focused) {
            pane.scroll_to_top();
        }
    }

    pub fn scroll_focused_to_bottom(&mut self) {
        if let Some(pane) = self.panes.get_mut(self.focused) {
            pane.scroll_to_bottom();
        }
    }

    /// Start a drag selection anchored at `(x, y)` inside pane `pane`
    /// (content row already resolved by the caller). A selection lives in
    /// ONE pane: moving into another pane's band keeps the anchor pane.
    pub fn begin_selection(&mut self, pane: usize, x: u16, y: u16, content_y: i32) {
        self.selection_pane = Some(pane);
        self.selection_anchor_content_y = content_y;
        self.selection_focus_content_y = content_y;
        self.selection_mouse_down_scroll = self
            .panes
            .get(pane)
            .map_or(0, |p| p.effective_scroll);
        self.drag_selection = Some((x, y, x, y));
    }

    /// Extend the active drag selection to `(x, y)` in the anchor pane.
    pub fn update_selection(&mut self, x: u16, y: u16, content_y: i32) {
        if self.selection_pane.is_none() {
            return;
        }
        self.selection_focus_content_y = content_y;
        if let Some((sx, sy, _, _)) = self.drag_selection {
            self.drag_selection = Some((sx, sy, x, y));
        }
    }

    /// Extend the active drag selection to a SCREEN point, resolving the
    /// content row through the anchor pane's recorded rect and render scroll
    /// (the mouse dispatcher's entry point during a drag).
    pub fn update_selection_at(&mut self, x: u16, y: u16) {
        let Some(pane) = self.selection_pane else {
            return;
        };
        let Some(rect) = self.pane_rects.get(pane).copied() else {
            return;
        };
        let content_y = self
            .panes
            .get(pane)
            .map_or(0, |p| p.content_row_at(rect, y));
        self.update_selection(x, y, content_y);
    }

    /// Whether a drag selection is active.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection_pane.is_some() && self.drag_selection.is_some()
    }

    /// Whether the active selection covers a real RANGE (the pointer moved
    /// since the anchor). A bare click anchors and releases on the same
    /// point: the event layer distinguishes drag from click, and a click
    /// must never copy the one character under it — same rule as the chat's
    /// copy-on-drag-release.
    #[must_use]
    pub fn is_drag(&self) -> bool {
        matches!(self.drag_selection, Some((sx, sy, fx, fy)) if sx != fx || sy != fy)
    }

    /// Drop the active selection without copying.
    pub fn cancel_selection(&mut self) {
        self.selection_pane = None;
        self.drag_selection = None;
        self.selection_anchor_content_y = 0;
        self.selection_focus_content_y = 0;
        self.selection_mouse_down_scroll = 0;
    }

    /// Extract the text spanned by the active selection out of the pane's
    /// painted regions (the chat's flow-based algorithm, scoped to the
    /// anchor pane so a drag never leaks across panes).
    #[must_use]
    pub fn extract_selected_text(&self) -> String {
        let Some(pane) = self.selection_pane else {
            return String::new();
        };
        let Some((sx, sy, fx, _fy)) = self.drag_selection else {
            return String::new();
        };
        let Some(regions) = self.pane_regions.get(pane) else {
            return String::new();
        };
        // The anchor used the scroll at mouse-down, the focus the current
        // scroll, so the selection survives scrolling during the drag.
        let content_anchor = i32::from(sy)
            .saturating_sub(i32::from(self.pane_body_top(pane)))
            .saturating_add(i32::from(self.selection_mouse_down_scroll));
        let content_focus = self.selection_focus_content_y;
        let start_content_y = content_anchor.min(content_focus);
        let end_content_y = content_anchor.max(content_focus);
        let (start_x, end_x) = if content_anchor <= content_focus {
            (sx, fx)
        } else {
            (fx, sx)
        };
        extract_text_in_region(regions, start_content_y, end_content_y, start_x, end_x)
    }

    /// Screen row the pane's body starts at (below its header), from the
    /// LAST recorded rect.
    fn pane_body_top(&self, pane: usize) -> u16 {
        self.pane_rects
            .get(pane)
            .map_or(0, |r| r.y + 1)
    }

    /// Whether any pane has a running sub-agent turn.
    #[must_use]
    pub fn any_running(&self) -> bool {
        self.panes.iter().any(|p| p.running)
    }

    /// Apply one sub-agent event to pane `pane`'s activity stream. Out-of
    /// range panes are ignored (the daemon is a normal pane for events too).
    pub fn apply_event(&mut self, pane: usize, event: &SubagentEvent) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.activity.apply(event);
        }
    }

    /// Set the running flag of pane `pane` (out-of-range panes ignored).
    pub fn set_running(&mut self, pane: usize, running: bool) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.running = running;
        }
    }

    /// Attach a cooperative stop flag to pane `pane`.
    pub fn set_stop_signal(&mut self, pane: usize, signal: Option<Arc<AtomicBool>>) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.stop_signal = signal;
        }
    }

    /// Draft text of pane `pane` (empty string when out of range).
    #[must_use]
    pub fn draft(&self, pane: usize) -> &str {
        self.drafts.get(pane).map(String::as_str).unwrap_or("")
    }

    /// Replace the draft text of pane `pane` (out-of-range panes ignored).
    pub fn set_draft(&mut self, pane: usize, text: String) {
        if let Some(d) = self.drafts.get_mut(pane) {
            *d = text;
        }
    }
}

/// Compute the pane rects for `pane_count` panes inside `area`.
///
/// Heuristic (documented at module level; NOT a perfect bin-packing):
/// - 1 pane: the whole area.
/// - `n >= 2`: pane 0 (daemon) is a full-width strip on top, height
///   `clamp(area.height / n, 3, 8)` shrunk when the area cannot also fit the
///   separator row plus one agent row. The remaining agents tile the rest:
///   a single row of equal columns while every column stays >=
///   [`MIN_COLUMN_WIDTH`] wide (with 1-column gaps), otherwise two balanced
///   rows — `ceil(m/2)` panes on top, `floor(m/2)` below, and when the
///   available height is odd the TOP row takes the extra row.
/// - Gaps are exactly 1 cell (row/column) between panes; there are no
///   double borders.
///
/// Returns exactly `pane_count` rects (empty for 0), all inside `area` and
/// pairwise non-overlapping. Degenerate areas may clamp panes to zero
/// height/width; the last row may be narrower than the others.
#[must_use]
pub fn compute_layout(pane_count: usize, area: Rect) -> Vec<Rect> {
    if pane_count == 0 {
        return Vec::new();
    }
    if pane_count == 1 {
        return vec![area];
    }

    // ── Daemon strip (pane 0) ──
    // Proportional to the pane count, clamped, and shrunk when the area is
    // too small for strip + separator + at least one agent row.
    let daemon_h = (area.height / pane_count as u16)
        .clamp(DAEMON_MIN_HEIGHT, DAEMON_MAX_HEIGHT)
        .min(area.height.saturating_sub(2).max(1));
    let daemon = Rect::new(area.x, area.y, area.width, daemon_h);

    let rest_y = area.y + daemon_h + GAP;
    let rest_h = area.bottom().saturating_sub(rest_y);
    let rest_w = area.width;

    // ── Agent panes (1..n) below the strip ──
    let agents = pane_count - 1;
    let single_row = agents == 1 || {
        let usable = rest_w.saturating_sub((agents - 1) as u16 * GAP);
        usable / agents as u16 >= MIN_COLUMN_WIDTH
    };

    let mut out = vec![daemon];
    if single_row {
        out.extend(split_columns(area.x, rest_y, rest_w, rest_h, agents));
    } else {
        let top_count = agents.div_ceil(2);
        let bottom_count = agents - top_count;
        // Balanced rows; the top row takes the extra row of an odd height.
        // Below 3 rows the separator cannot fit — split what is left.
        let (top_h, bottom_h, row_gap) = if rest_h >= 2 * GAP + 2 {
            let usable = rest_h - GAP;
            let top = usable.div_ceil(2);
            (top, usable - top, GAP)
        } else {
            let top = rest_h.min(1);
            (top, rest_h - top, 0)
        };
        out.extend(split_columns(area.x, rest_y, rest_w, top_h, top_count));
        out.extend(split_columns(
            area.x,
            rest_y + top_h + row_gap,
            rest_w,
            bottom_h,
            bottom_count,
        ));
    }
    out
}

/// Split `[x, x + total_w)` into `count` equal columns separated by one
/// gap column; the first `total_w % count` columns absorb the remainder.
fn split_columns(x: u16, y: u16, total_w: u16, h: u16, count: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let gaps = (count - 1) as u16;
    if total_w < gaps + 1 {
        // Degenerate: not even one column of width per pane.
        return (0..count)
            .map(|i| Rect::new(x.saturating_add(i as u16), y, 0, h))
            .collect();
    }
    let usable = total_w - gaps;
    let per = usable / count as u16;
    let rem = usable % count as u16;
    let mut out = Vec::with_capacity(count);
    let mut cx = x;
    for i in 0..count {
        let w = per + u16::from((i as u16) < rem);
        out.push(Rect::new(cx, y, w, h));
        cx = cx.saturating_add(w).saturating_add(GAP);
    }
    out
}

/// Render the split view: compute the layout, record the hit rects on the
/// state, then paint each pane (header + activity tail).
pub fn render(buf: &mut Buffer, area: Rect, state: &mut AgentsSplitState, theme: &Theme) {
    let rects = compute_layout(state.pane_count(), area);
    state.pane_rects = rects.clone();
    // Every pane gets a region slot for THIS frame: panes that do not
    // render (degenerate rects) contribute an empty list, so extraction
    // over them finds nothing instead of reading a stale frame's regions.
    state.pane_regions.clear();
    state.pane_regions.resize(state.pane_count(), Vec::new());
    let focused = state.focused;
    // Split the borrows once: the per-pane loop needs `&mut panes[i]`,
    // `&mut block_cache` and `&mut pane_regions[i]` simultaneously (the
    // cache is keyed per pane, the regions per pane slot).
    let AgentsSplitState {
        panes,
        block_cache,
        pane_regions,
        ..
    } = state;
    // Live-key collection, the right panel's cache-hygiene pattern
    // (`live_block_keys` + `retain`): every markdown block drawn THIS frame
    // reports its key; anything not reported is dead (streamed past the
    // tail, or the pane vanished) and its renderer+scratch are dropped
    // instead of pinning hundreds of KB per pane for the session's life.
    let mut live_keys: HashSet<String> = HashSet::new();
    for (i, rect) in rects.into_iter().enumerate() {
        let Some(pane) = panes.get_mut(i) else {
            break;
        };
        render_pane(
            buf,
            rect,
            pane_hotkey(i),
            i,
            pane,
            i == focused,
            theme,
            block_cache,
            &mut live_keys,
            &mut pane_regions[i],
        );
    }
    block_cache.retain(|key, _| live_keys.contains(key));
    // Copy-by-selection highlight, painted LAST so it inverts the final
    // cells (the right panel paints its highlight the same way).
    highlight_selection(buf, state);
}

/// Paint one pane: element background, hotkey header (`[n] agent ●`) and
/// the tail of the activity stream (or the idle hint) — rendered through
/// the right panel's REAL pipeline (`activity_lines` + `SubagentBlockRenderer`),
/// so markdown blocks wrap, style and cache exactly like the sub-agent
/// mini-chat instead of a stripped-down mirror.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    buf: &mut Buffer,
    rect: Rect,
    hotkey: Option<char>,
    pane_index: usize,
    pane: &mut AgentPane,
    focused: bool,
    theme: &Theme,
    block_cache: &mut HashMap<String, SubagentBlockRenderer>,
    live_keys: &mut HashSet<String>,
    out_regions: &mut Vec<TextRegion>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    // Background fill (element color, like the right panel's boxes).
    let bg = Style::default().bg(rgba_color(theme.background_element));
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(bg);
            }
        }
    }

    // ── Header row ──
    let key = hotkey.map_or('?', |c| c);
    let dot = if pane.running { '●' } else { '○' };
    let label = format!("[{key}] {} ", pane.agent);
    let label_w = label.chars().count() as u16;
    if focused {
        // Highlighted header: accent-tinted band with readable contrast.
        let band = blend(theme.background_element, theme.accent, 0.35);
        let style = Style::default()
            .fg(rgba_color(contrast_fg(band)))
            .bg(rgba_color(band))
            .add_modifier(Modifier::BOLD);
        draw_text(buf, &label, rect.x, rect.y, rect.width, style);
        draw_char(buf, dot, rect.x + label_w, rect.y, style);
    } else {
        let style = Style::default().fg(rgba_color(theme.text_muted));
        draw_text(buf, &label, rect.x, rect.y, rect.width, style);
        let dot_style = if pane.running {
            Style::default().fg(rgba_color(theme.success))
        } else {
            style
        };
        draw_char(buf, dot, rect.x + label_w, rect.y, dot_style);
    }

    // ── Body: the right panel's drawable pipeline (markdown included) ──
    let body_y = rect.y + 1;
    let body_h = rect.bottom().saturating_sub(body_y);
    // Same wrap rule as the right panel's subagent section: width minus the
    // left+right padding, so `estimate_height` agrees with the drawn rows.
    let wrap_w = rect.width.saturating_sub(2);
    if body_h == 0 || wrap_w == 0 {
        return;
    }
    let lines = activity_lines(&pane.activity, wrap_w);
    if lines.is_empty() {
        let hint = "idle — waiting for the next prompt";
        draw_text(
            buf,
            hint,
            rect.x + 1,
            body_y,
            rect.width.saturating_sub(1),
            Style::default().fg(rgba_color(theme.text_muted)),
        );
        // The empty pane measures zero content: a stale scroll offset has
        // nothing to hold on to.
        pane.effective_scroll = 0;
        pane.content_height = 0;
        pane.body_height = body_h;
        return;
    }

    // Content height in VISUAL rows — the scroll ceiling, computed from the
    // same `visual_rows` the renderer consumes so they can never disagree.
    let content_height: u16 = lines
        .iter()
        .map(|l| l.visual_rows(wrap_w))
        .fold(0u16, u16::saturating_add);
    let max_scroll = content_height.saturating_sub(body_h);
    // Sticky bottom (the chat's follow contract): a following pane shows the
    // newest rows; a scrolled-up pane keeps its offset, clamped to content.
    let scroll_y = if pane.auto_follow {
        max_scroll
    } else {
        pane.scroll_y.min(max_scroll)
    };
    pane.effective_scroll = scroll_y;
    pane.content_height = content_height;
    pane.body_height = body_h;

    // Sliding window: walk the lines with their absolute content row, skip
    // the ones fully above `scroll_y`, paint the ones intersecting the body
    // (passing `skip` so a straddling block shows only its lower part) and
    // label every painted visual row with its CONTENT row — the selection
    // regions' coordinate system, exactly the right panel's contract.
    let theme_key = subagent_theme_key(theme);
    let tint = theme.background_element;
    let mut regions: Vec<TextRegion> = Vec::new();
    let mut content_row: u16 = 0;
    for (index, line) in lines.iter().enumerate() {
        let h = line.visual_rows(wrap_w);
        let line_end = content_row.saturating_add(h);
        if line_end <= scroll_y {
            // Entirely above the window.
            content_row = line_end;
            continue;
        }
        if content_row >= scroll_y.saturating_add(body_h) {
            break;
        }
        let skip = scroll_y.saturating_sub(content_row);
        // A block straddling the window top has content_row < scroll_y: the
        // screen position clamps to the body top and `skip` carries the
        // scrolled-off part (saturating — the raw difference underflows).
        let screen_y = body_y + content_row.saturating_sub(scroll_y);
        let draw_rows = h
            .saturating_sub(skip)
            .min(rect.bottom().saturating_sub(screen_y));
        if draw_rows > 0 {
            draw_activity_line(
                buf,
                line,
                index,
                pane_index,
                rect.x + 1,
                screen_y,
                wrap_w,
                skip,
                draw_rows as usize,
                focused,
                theme,
                theme_key,
                tint,
                block_cache,
                live_keys,
                &mut regions,
                content_row,
            );
        }
        content_row = line_end;
    }
    *out_regions = regions;
}

/// Paint ONE drawable activity line at absolute `(x, y)`, clipped to
/// `draw_rows` with its first `skip` rows scrolled off. Markdown blocks
/// (`Message`/`Thought`) go through the pane's persistent
/// `SubagentBlockRenderer` (chat-style incremental parsing, scratch-buffer
/// blit — the exact pattern of the right panel's live window); Tool/Diff/
/// Plan lines draw with the same marker/glyph styles. Every painted visual
/// row is reported as a `TextRegion` labeled with its CONTENT row, so
/// copy-by-selection reads exactly what is on screen.
#[allow(clippy::too_many_arguments)]
fn draw_activity_line(
    buf: &mut Buffer,
    line: &SubagentActivityLine,
    index: usize,
    pane_index: usize,
    x: u16,
    y: u16,
    wrap_w: u16,
    skip: u16,
    draw_rows: usize,
    focused: bool,
    theme: &Theme,
    theme_key: u64,
    tint: RGBA,
    block_cache: &mut HashMap<String, SubagentBlockRenderer>,
    live_keys: &mut HashSet<String>,
    regions: &mut Vec<TextRegion>,
    content_row: u16,
) {
    let text_style = if focused {
        Style::default().fg(rgba_color(theme.text))
    } else {
        Style::default().fg(rgba_color(theme.text_muted))
    };
    let dimmed_style = Style::default().fg(rgba_color(theme.text_muted));
    let marker_style_ok = Style::default().fg(rgba_color(theme.success));
    let marker_style_err = Style::default().fg(rgba_color(theme.error));

    match line {
        SubagentActivityLine::Tool { rows, status, .. } => {
            let marker = match status {
                ToolCallStatus::Completed => "✓",
                ToolCallStatus::Failed => "✗",
                ToolCallStatus::InProgress => "·",
                _ => "·",
            };
            let marker_style = match status {
                ToolCallStatus::Completed => marker_style_ok,
                ToolCallStatus::Failed => marker_style_err,
                _ => dimmed_style,
            };
            for di in 0..draw_rows {
                let ri = skip as usize + di;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + di as u16;
                if ri == 0 {
                    draw_text(buf, marker, x, line_y, wrap_w, marker_style);
                }
                draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), text_style);
                let region_text = if ri == 0 {
                    format!("{marker} {seg}")
                } else {
                    format!("  {seg}")
                };
                regions.push(TextRegion::one_row(
                    i32::from(content_row.saturating_add(ri as u16)),
                    x,
                    x + wrap_w,
                    region_text,
                ));
            }
        }
        SubagentActivityLine::Diff(rows) => {
            for di in 0..draw_rows {
                let ri = skip as usize + di;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + di as u16;
                draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), dimmed_style);
                regions.push(TextRegion::one_row(
                    i32::from(content_row.saturating_add(ri as u16)),
                    x,
                    x + wrap_w,
                    format!("  {seg}"),
                ));
            }
        }
        SubagentActivityLine::Plan { status, rows } => {
            let (glyph, style) = match status {
                PlanEntryStatus::Completed => ("☑", marker_style_ok),
                PlanEntryStatus::InProgress => ("☐", text_style),
                _ => ("☐", dimmed_style),
            };
            for di in 0..draw_rows {
                let ri = skip as usize + di;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + di as u16;
                if ri == 0 {
                    draw_text(buf, glyph, x, line_y, wrap_w, style);
                }
                draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), style);
                let region_text = if ri == 0 {
                    format!("{glyph} {seg}")
                } else {
                    format!("  {seg}")
                };
                regions.push(TextRegion::one_row(
                    i32::from(content_row.saturating_add(ri as u16)),
                    x,
                    x + wrap_w,
                    region_text,
                ));
            }
        }
        SubagentActivityLine::Message { text, .. } | SubagentActivityLine::Thought { text, .. } => {
            // Persistent chat-style renderer per (pane, timeline) block —
            // `set_content` re-parses only the streamed tail. Thought is
            // dimmed, exactly like the right panel's mini-chat.
            let dimmed = matches!(line, SubagentActivityLine::Thought { .. });
            let fg = if dimmed { theme.text_muted } else { theme.text };
            let key = format!("{pane_index}:{index}");
            // This block is drawn THIS frame: its cache entry is live (the
            // split-level `retain` drops every key not reported here).
            live_keys.insert(key.clone());
            let stale = block_cache.get(&key).is_none_or(|e| {
                e.wrap_w != wrap_w || e.theme_key != theme_key || e.bg != tint || e.dimmed != dimmed
            });
            if stale {
                let mut md = MarkdownRenderable::new(Some(text.clone()));
                md.set_fg(Some(ColorInput::RGBA(fg)));
                md.set_bg(Some(ColorInput::RGBA(tint)));
                crate::util::markdown::apply_theme(&mut md, theme);
                let area = Rect::new(0, 0, wrap_w, h_from(text, wrap_w));
                block_cache.insert(
                    key.clone(),
                    SubagentBlockRenderer {
                        wrap_w,
                        theme_key,
                        bg: tint,
                        dimmed,
                        text: text.clone(),
                        md,
                        scratch: Buffer::empty(area),
                    },
                );
            }
            let entry = block_cache
                .get_mut(&key)
                .expect("block cache entry was just inserted or is valid");
            if entry.text != *text {
                entry.md.set_content(text.clone());
                entry.text = text.clone();
            }
            let area = Rect::new(0, 0, wrap_w, h_from(text, wrap_w));
            if entry.scratch.area() != &area {
                entry.scratch.resize(area);
            }
            // `render_self` fills the whole area with the background first.
            entry.md.render_self(&mut entry.scratch, area);
            // Blit the VISIBLE band of the block: rows `skip..skip+draw_rows`
            // (the scroll window decides which band; at sticky bottom an
            // oversized block naturally shows its tail). Every painted row
            // becomes a content-row region sampled from the same cells, so
            // extraction matches the display.
            for di in 0..draw_rows {
                let sy = (skip as usize + di) as u16;
                for cx in 0..wrap_w {
                    let Some(src) = entry.scratch.cell((cx, sy)) else {
                        break;
                    };
                    let Some(dst) = buf.cell_mut((x + cx, y + di as u16)) else {
                        break;
                    };
                    dst.set_symbol(src.symbol());
                    dst.set_style(src.style());
                }
                let row_cells: Vec<ratatui::buffer::Cell> = (0..wrap_w)
                    .map(|dx| entry.scratch.cell((dx, sy)).cloned().unwrap_or_default())
                    .collect();
                let trimmed = text_from_cell_row(&row_cells, wrap_w as usize);
                if !trimmed.is_empty() {
                    regions.push(TextRegion::one_row(
                        i32::from(content_row.saturating_add(sy)),
                        x,
                        x + wrap_w,
                        trimmed,
                    ));
                }
            }
        }
    }
}

/// Block height at `wrap_w` — the SAME estimator the height math uses, so
/// the scratch buffer always matches the clipped blit.
fn h_from(text: &str, wrap_w: u16) -> u16 {
    cosh_tui::core::renderables::markdown::estimate_height(text, wrap_w).max(1)
}

/// Draw `text` clipped to `max_w` columns, filtering control characters.
fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    for (i, ch) in text.chars().filter(|c| !c.is_control()).enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        let Some(cx) = x.checked_add(i) else { break };
        if cx >= x.saturating_add(max_w) {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Draw a single glyph (status dot, focus rail).
fn draw_char(buf: &mut Buffer, ch: char, x: u16, y: u16, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

/// Copy-by-selection highlight: invert the cells under the drag selection
/// (the chat's flow-based algorithm, scoped to the ANCHOR pane so a drag
/// never paints into a neighbour). The anchor is re-projected with the
/// scroll it was taken at, the focus with the CURRENT scroll, so the visual
/// band survives scrolling mid-drag — exactly the right panel's contract.
/// Painted after the pane content so it inverts the final cells.
fn highlight_selection(buf: &mut Buffer, state: &AgentsSplitState) {
    let Some((sx, _sy, fx, _fy)) = state.drag_selection else {
        return;
    };
    let Some(pane_index) = state.selection_pane else {
        return;
    };
    let Some(rect) = state.pane_rects.get(pane_index) else {
        return;
    };
    let Some(pane) = state.panes.get(pane_index) else {
        return;
    };
    let body_top = i32::from(rect.y + 1);
    let body_last = i32::from(rect.bottom().saturating_sub(1));
    if body_last < body_top {
        return;
    }
    // Body band: content starts one column in (the pane's left rail), the
    // right edge excludes the pane's own padding column.
    let min_x = rect.x.saturating_add(1);
    let max_x = rect.right().saturating_sub(1);
    let anchor_content = state.selection_anchor_content_y;
    let focus_content = state.selection_focus_content_y;
    let anchor_screen = anchor_content
        .saturating_sub(i32::from(state.selection_mouse_down_scroll))
        .saturating_add(body_top)
        .clamp(body_top, body_last) as u16;
    let focus_screen = focus_content
        .saturating_sub(i32::from(pane.effective_scroll))
        .saturating_add(body_top)
        .clamp(body_top, body_last) as u16;
    let start_y = anchor_screen.min(focus_screen);
    let end_y = anchor_screen.max(focus_screen);
    let (top_x, bottom_x) = if anchor_content <= focus_content {
        (sx, fx)
    } else {
        (fx, sx)
    };
    let top_content = anchor_content.min(focus_content);
    let bottom_content = anchor_content.max(focus_content);

    for cy in start_y..=end_y {
        // Content row under this screen row. Rows outside the true content
        // span (selection clamped off-screen) are not part of it — skip.
        let row_content = i32::from(cy)
            .saturating_sub(body_top)
            .saturating_add(i32::from(pane.effective_scroll));
        if row_content < top_content || row_content > bottom_content {
            continue;
        }
        let (lx1, lx2) = if row_content == top_content && row_content == bottom_content {
            (top_x.min(bottom_x), top_x.max(bottom_x))
        } else if row_content == top_content {
            (top_x, max_x)
        } else if row_content == bottom_content {
            (min_x, bottom_x)
        } else {
            (min_x, max_x)
        };
        let lx1 = lx1.max(min_x);
        let lx2 = lx2.min(max_x);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::session::right_panel::types::SubagentTimelineEntry;
    use crate::theme::ThemeRegistry;

    fn test_theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    /// Layout invariants for one (n, area) pair: exact pane count, containment,
    /// no overlap, daemon strip on top, and every cell covered at most once
    /// with only a small gap budget left uncovered.
    fn assert_layout_ok(pane_count: usize, area: Rect) {
        let rects = compute_layout(pane_count, area);
        assert_eq!(rects.len(), pane_count, "n={pane_count} {area:?}");

        for r in &rects {
            assert!(r.x >= area.x && r.y >= area.y, "inside: {r:?} in {area:?}");
            assert!(
                r.right() <= area.right() && r.bottom() <= area.bottom(),
                "inside: {r:?} in {area:?}"
            );
        }
        for i in 0..pane_count {
            for j in (i + 1)..pane_count {
                assert_eq!(
                    rects[i].intersection(rects[j]).area(),
                    0,
                    "no overlap: {rects:?}"
                );
            }
        }
        if pane_count >= 2 {
            let daemon = rects[0];
            assert_eq!(daemon.y, area.y, "pane 0 on top");
            assert_eq!(daemon.x, area.x);
            assert_eq!(daemon.width, area.width, "daemon is a full strip");
            assert!(daemon.height < area.height, "daemon is not the whole area");
            assert!(
                (1..=DAEMON_MAX_HEIGHT).contains(&daemon.height),
                "daemon height clamped: {daemon:?}"
            );
        }

        // Cell coverage: every cell in at most one pane; the uncovered cells
        // are the 1-cell gaps (small fraction of the area).
        let w = area.width as usize;
        let h = area.height as usize;
        let mut grid = vec![0u32; w * h];
        for r in &rects {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let idx = (y - area.y) as usize * w + (x - area.x) as usize;
                    grid[idx] += 1;
                }
            }
        }
        assert!(
            grid.iter().all(|&c| c <= 1),
            "every cell covered at most once: n={pane_count} {area:?}"
        );
        let uncovered = grid.iter().filter(|&&c| c == 0).count();
        let total = w * h;
        assert!(
            uncovered * 5 < total,
            "gaps must stay small: {uncovered}/{total} uncovered, n={pane_count} {area:?}"
        );
        // And the covered cells exactly account for the pane rects.
        let sum: u32 = rects.iter().map(|r| u32::from(r.area())).sum();
        assert_eq!(
            sum + uncovered as u32,
            total as u32,
            "sum of pane areas + gaps must equal the area"
        );
    }

    #[test]
    fn layout_tiles_area_without_overlap_for_n2_to_7() {
        for (w, hh) in [(80u16, 24u16), (120, 40), (60, 20)] {
            let area = Rect::new(3, 5, w, hh);
            for n in 2..=7 {
                assert_layout_ok(n, area);
            }
        }
    }

    #[test]
    fn single_pane_takes_the_whole_area() {
        let area = Rect::new(2, 2, 80, 24);
        assert_eq!(compute_layout(1, area), vec![area]);
        assert!(compute_layout(0, area).is_empty());
    }

    #[test]
    fn two_panes_split_into_strip_and_full_width_agent() {
        let area = Rect::new(0, 0, 80, 24);
        let rects = compute_layout(2, area);
        assert_eq!(rects[0].width, 80, "daemon strip full width");
        assert_eq!(rects[1].width, 80, "lone agent keeps the full width");
        assert_eq!(rects[1].y, rects[0].bottom() + GAP, "one gap row between");
        assert_eq!(rects[1].bottom(), area.bottom(), "agent reaches the bottom");
    }

    #[test]
    fn narrow_area_falls_back_to_two_rows() {
        // 6 agents at 60 cols: per-column width would be ~9 < 24 → 2 rows.
        let area = Rect::new(0, 0, 60, 20);
        let rects = compute_layout(7, area); // daemon + 6 agents
        let row_tops: Vec<u16> = rects[1..].iter().map(|r| r.y).collect();
        let distinct: Vec<u16> = {
            let mut v = row_tops.clone();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(distinct.len(), 2, "two agent rows, tops {row_tops:?}");
        // The top row carries ceil(6/2) = 3 panes, the bottom floor(6/2) = 3.
        assert_eq!(
            row_tops.iter().filter(|&&y| y == distinct[0]).count(),
            3,
            "top row = ceil(n/2)"
        );
        assert_eq!(
            row_tops.iter().filter(|&&y| y == distinct[1]).count(),
            3,
            "bottom row = floor(n/2)"
        );
    }

    #[test]
    fn hotkey_labels_follow_pane_order() {
        assert_eq!(pane_hotkey(0), Some('0'));
        assert_eq!(pane_hotkey(1), Some('1'));
        assert_eq!(pane_hotkey(9), Some('9'));
        assert_eq!(pane_hotkey(10), None);
    }

    #[test]
    fn new_inserts_daemon_first_and_keeps_picker_order() {
        let state = AgentsSplitState::new(vec!["kilo".into(), "claude".into()]);
        assert_eq!(state.pane_count(), 3);
        assert_eq!(state.panes[0].agent, "cosh");
        assert_eq!(state.panes[1].agent, "kilo");
        assert_eq!(state.panes[2].agent, "claude");
        assert_eq!(state.drafts.len(), 3, "drafts aligned with panes");

        // Already-first daemon is not duplicated.
        let state = AgentsSplitState::new(vec![DAEMON_LABEL.into(), "claude".into()]);
        assert_eq!(state.pane_count(), 2);
        assert_eq!(state.panes[0].agent, "cosh");
    }

    #[test]
    fn focus_hotkey_targets_daemon_and_agents() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        assert!(state.focus_hotkey('2'));
        assert_eq!(state.focused, 2);
        assert_eq!(state.focused_pane().map(|p| p.agent.as_str()), Some("kilo"));
        assert!(state.focus_hotkey('0'));
        assert_eq!(state.focused, 0);
        // Out-of-range and unknown keys leave the focus alone.
        assert!(!state.focus_hotkey('9'));
        assert_eq!(state.focused, 0);
        assert!(!state.focus_hotkey('x'));
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn focus_next_prev_wrap_around() {
        let mut state = AgentsSplitState::new(vec!["claude".into()]);
        assert_eq!(state.pane_count(), 2);
        state.focus_next();
        assert_eq!(state.focused, 1);
        state.focus_next();
        assert_eq!(state.focused, 0, "wraps around");
        state.focus_prev();
        assert_eq!(state.focused, 1);
    }

    #[test]
    fn apply_event_accumulates_message_chunks_in_the_right_pane() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        state.apply_event(
            1,
            &SubagentEvent::Message {
                text: "hello ".to_string(),
            },
        );
        state.apply_event(
            1,
            &SubagentEvent::Message {
                text: "world".to_string(),
            },
        );
        state.apply_event(
            2,
            &SubagentEvent::Message {
                text: "other pane".to_string(),
            },
        );

        // The daemon pane (0) stays empty; pane 1 coalesced its two chunks.
        assert!(state.panes[0].activity.timeline.is_empty());
        assert_eq!(state.panes[1].activity.timeline.len(), 1);
        let SubagentTimelineEntry::Message { text } = &state.panes[1].activity.timeline[0] else {
            panic!("expected a Message entry");
        };
        assert!(text.contains("hello") && text.contains("world"), "got {text:?}");
        assert_eq!(state.panes[2].activity.timeline.len(), 1);
    }

    #[test]
    fn apply_event_ignores_out_of_range_panes() {
        let mut state = AgentsSplitState::new(vec!["claude".into()]);
        state.apply_event(
            7,
            &SubagentEvent::Message {
                text: "lost".to_string(),
            },
        );
        assert!(state.panes.iter().all(|p| p.activity.timeline.is_empty()));
    }

    #[test]
    fn any_running_tracks_the_running_flag() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        assert!(!state.any_running());
        state.set_running(2, true);
        assert!(state.any_running());
        state.set_running(2, false);
        assert!(!state.any_running());
        // Out-of-range set_running is a no-op.
        state.set_running(9, true);
        assert!(!state.any_running());
    }

    #[test]
    fn render_paints_headers_and_idle_hint() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        assert_eq!(state.pane_rects.len(), 3, "hit rects recorded for mouse");

        let row_text = |y: u16| {
            (0..80)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect::<String>()
        };
        let all: String = (0..24).map(row_text).collect::<Vec<_>>().join("\n");
        assert!(all.contains("[0] cosh"), "daemon header: {all:?}");
        assert!(all.contains("[1] claude"), "agent header: {all:?}");
        assert!(all.contains("[2] kilo"), "agent header: {all:?}");
        assert!(all.contains('○'), "idle panes show the hollow dot");
        assert!(
            all.contains("idle — waiting for the next prompt"),
            "idle hint rendered: {all:?}"
        );
    }

    #[test]
    fn render_running_pane_shows_the_filled_dot_and_activity_tail() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into()]);
        state.set_running(1, true);
        // More lines than the pane body can hold, so the TAIL scroll is
        // actually exercised (the daemon strip is 8 rows at n=2, leaving a
        // ~14-row agent body).
        for i in 0..40 {
            state.apply_event(
                1,
                &SubagentEvent::Message {
                    text: format!("line {i} of the stream\n"),
                },
            );
        }
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let all: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains('●'), "running pane shows the filled dot: {all:?}");
        // Tail scroll: the FIRST lines scrolled off, the LAST stay visible.
        assert!(!all.contains("line 0 "), "old lines scrolled off: {all:?}");
        assert!(!all.contains("line 25 "), "old lines scrolled off: {all:?}");
        assert!(
            all.contains("line 39 of the stream"),
            "newest line visible: {all:?}"
        );
    }
}
