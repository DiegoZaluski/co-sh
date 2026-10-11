//! Split-view "panes" for the `/agents` route: one pane per agent the user
//! picked (the `cosh` daemon pane only when explicitly marked), laid out
//! over the session area with a simple bin-packing heuristic.
//!
//! This module owns ONLY the pane model + layout + painting. The prompt is
//! a single shared input that lives outside this area (`compute_prompt_area`
//! in `app/render.rs`), so dispatching a draft to a pane is not this
//! module's concern — it only keeps the per-pane drafts so the dispatcher
//! can read the focused pane's target.
//!
//! Layout heuristic (deliberately approximate — "fits nicely", not a perfect
//! bin-packing): EVERY pane — the daemon included — is an equal participant
//! in the area division: one row of equal columns while each column stays
//! readable (>= 24 cols), otherwise two balanced rows (ceil/floor split, the
//! top row slightly taller when the height is odd). Pane order is preserved,
//! so marked agents keep the picker order; when the daemon is picked it
//! keeps its grid position among them. The last row may end up
//! narrower/taller than the others; what matters is that the panes tile the
//! area with only 1-cell gaps and no holes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;
use cosh_tools::subagent::events::{PlanEntryStatus, SubagentEvent, ToolCallStatus};
use cosh_tools::subagent::palette::{display_name, palette_for};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::component::spinner::SpinnerState;
use crate::routes::session::right_panel::subagent_theme_key;
use crate::routes::session::right_panel::types::{
    activity_lines, SubagentActivity, SubagentActivityLine, SubagentBlockRenderer,
};
use crate::theme::{Theme, blend, contrast_fg, luminance, rgba_color};
use crate::util::text_region::{TextRegion, extract_text_in_region, text_from_cell_row};

/// Display label of the daemon pane when the picker marks it: the TUI's
/// own agent. No pane carries it unless the user marked `cosh`.
pub const DAEMON_LABEL: &str = "cosh";

/// Minimum luminance distance between a brand color and the pane background
/// for the name to keep the pure brand color. Below it the color would
/// dissolve into the surface (white Codex on a near-white theme, dark
/// opencode ink on a dark theme), so it lifts toward the readable
/// contrast color instead.
pub(crate) const BRAND_CONTRAST_FLOOR: f32 = 40.0;

/// How far an UNFOCUSED pane's brand-colored name dims toward the element
/// background — the same register the old muted header used, so the
/// focused pane reads at a glance. The FOCUSED name keeps the pure brand
/// color: the brand mark is the identity, not a highlight to be dimmed.
const DIM_UNFOCUSED: f32 = 0.45;

/// Hotkey of the daemon pane. The remaining panes take `1`..=`9` in picker
/// order, so the daemon is always reachable with a single keypress.
pub const DAEMON_HOTKEY: char = '0';

/// Minimum readable width of one agent column before the layout falls back
/// to a second row (see the module-level heuristic description).
const MIN_COLUMN_WIDTH: u16 = 24;

/// Drag auto-scroll (the chat's fourth scroll form), mirrored from the
/// right panel: the pointer entering a pane's edge zone scrolls it, faster
/// the deeper into the zone it sits.
const AUTO_SCROLL_THRESHOLD: i32 = 3;
const AUTO_SCROLL_SPEED_SLOW: f64 = 6.0;
const AUTO_SCROLL_SPEED_MEDIUM: f64 = 36.0;
const AUTO_SCROLL_SPEED_FAST: f64 = 72.0;

/// Gap cells (1 column / 1 row) between neighbouring panes.
const GAP: u16 = 1;

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
    /// A cold-restore replay pass is IN FLIGHT for this pane (its bound
    /// session is being rehydrated through `session/load`). While set, a
    /// user prompt is refused with a toast (the pane's history is still
    /// arriving — a prompt would interleave with the replay stream), the
    /// mirror keeps spinning, and the header paints just the simple
    /// braille spinner at the pane's right edge. Cleared by the
    /// `AgentsPaneReplayed` handler, success or failure.
    pub rehydrating: bool,
    /// Per-pane animation state for the header's braille spinner
    /// (`component::spinner`).
    pub rehydrate_spinner: SpinnerState,
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
            rehydrating: false,
            rehydrate_spinner: SpinnerState::new(),
            scroll_y: 0,
            auto_follow: true,
            effective_scroll: 0,
            content_height: 0,
            body_height: 0,
        }
    }

    /// Build a pane from its persisted reference: the agent CLI plus, when
    /// the pane already ran once, the ACP session id it RESUMES (the
    /// `Bound` variant). A `Name` pane starts unbound — its first turn
    /// creates a session, and the done-handler promotes the persisted
    /// entry to `Bound`.
    #[must_use]
    pub fn from_ref(pane_ref: &crate::types::AgentPaneRef) -> Self {
        let mut pane = Self::new(pane_ref.agent().to_string());
        pane.session_id = pane_ref.session_id().map(str::to_string);
        pane
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
    /// One pane per agent in picker order. The `cosh` daemon pane exists
    /// only when the picker marked it, and only as an ordinary pane.
    pub panes: Vec<AgentPane>,
    /// Focused pane index (default: the first picked pane).
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
    /// Drag auto-scroll (the chat's fourth scroll form): armed while a drag
    /// selection holds the pointer in a pane's edge zone. The speed is
    /// re-derived on every drag move; `handle_auto_scroll` advances the
    /// anchor pane's scroll per frame.
    pub is_auto_scrolling: bool,
    auto_scroll_accumulator: f64,
    auto_scroll_speed: f64,
}

impl AgentsSplitState {
    /// Build the pane set from the user's split config, in the order they
    /// setup. NO daemon insert: the `cosh` pane exists ONLY when the picker
    /// marked it — the split is a plain reflection of the user's choice.
    /// Each entry carries its persisted binding (`AgentPaneRef::Bound`
    /// resumes its ACP session). `drafts` stays aligned with `panes`.
    #[must_use]
    pub fn new(agents: Vec<crate::types::AgentPaneRef>) -> Self {
        let panes: Vec<AgentPane> = agents
            .iter()
            .filter(|r| !r.agent().is_empty())
            .map(AgentPane::from_ref)
            .collect();
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
            is_auto_scrolling: false,
            auto_scroll_accumulator: 0.0,
            auto_scroll_speed: 0.0,
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

    /// Focus the pane bound to hotkey `ch` (`'0'` = the daemon pane WHEN
    /// present, `'1'..='9'` = the panes in picker order). Returns whether a
    /// pane matched.
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

    /// Scroll the pane whose rect contains screen position (`x`, `y`) (the
    /// wheel path, the right panel's `scroll_up_at` contract: the pane under
    /// the pointer moves, the others stay put). `false` when no pane owns
    /// the position. The grid layout places panes side by side sharing the
    /// same row band, so the column must be part of the hit-test — a row-only
    /// match lands every wheel on the FIRST pane of the row (the daemon).
    pub fn scroll_at(&mut self, x: u16, y: u16, rows: i32) -> bool {
        let Some(index) = self
            .pane_rects
            .iter()
            .position(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
        else {
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
        self.stop_auto_scroll();
    }

    /// Screen band of the anchor pane's body (below its header), from the
    /// LAST recorded rects. The auto-scroll zone geometry.
    fn selection_body_band(&self) -> Option<(i32, i32)> {
        let pane = self.selection_pane?;
        let rect = self.pane_rects.get(pane)?;
        let top = i32::from(rect.y + 1); // below the header
        let bottom = i32::from(rect.bottom());
        (top < bottom).then_some((top, bottom))
    }

    /// Current scroll of the anchor pane (0 when none).
    fn selection_scroll(&self) -> u16 {
        self.selection_pane
            .and_then(|pane| self.panes.get(pane))
            .map_or(0, |p| p.scroll_y)
    }

    fn auto_scroll_direction(&self, y: u16) -> i32 {
        let Some((top, bottom)) = self.selection_body_band() else {
            return 0;
        };
        if i32::from(y) <= top + AUTO_SCROLL_THRESHOLD && self.selection_scroll() > 0 {
            return -1;
        }
        if i32::from(y) >= bottom.saturating_sub(AUTO_SCROLL_THRESHOLD) {
            return 1;
        }
        0
    }

    fn auto_scroll_speed_for(&self, y: u16) -> f64 {
        let Some((top, bottom)) = self.selection_body_band() else {
            return 0.0;
        };
        let rel = (i32::from(y)).clamp(top, bottom.saturating_sub(1)) - top;
        let h = (bottom - top).max(1);
        let min_dist = rel.min(h - rel);
        if min_dist <= 1 {
            AUTO_SCROLL_SPEED_FAST
        } else if min_dist <= 2 {
            AUTO_SCROLL_SPEED_MEDIUM
        } else {
            AUTO_SCROLL_SPEED_SLOW
        }
    }

    /// Called on every drag move: arm auto-scroll when the pointer is near
    /// the anchor pane's edge, or stop it otherwise (the right panel's
    /// `update_auto_scroll` contract — the speed is re-derived on EVERY move
    /// so pushing deeper into the edge zone accelerates).
    pub fn update_auto_scroll(&mut self, y: u16) {
        let dir = self.auto_scroll_direction(y);
        if dir == 0 {
            self.stop_auto_scroll();
        } else {
            if !self.is_auto_scrolling {
                self.is_auto_scrolling = true;
                self.auto_scroll_accumulator = 0.0;
            }
            self.auto_scroll_speed = self.auto_scroll_speed_for(y);
        }
    }

    /// Advance drag auto-scroll by `delta_time` (once per render frame).
    /// Clamps the anchor pane's scroll and stops at the content bounds.
    pub fn handle_auto_scroll(&mut self, delta_time: f64) {
        if !self.is_auto_scrolling {
            return;
        }
        let Some(pane) = self.selection_pane else {
            self.stop_auto_scroll();
            return;
        };
        let dir = self
            .drag_selection
            .map_or(0, |(_, _, _, fy)| self.auto_scroll_direction(fy));
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
        if let Some(p) = self.panes.get_mut(pane)
            && !p.scroll_by(int_scroll)
        {
            // Hit a content bound: the selection can no longer grow in
            // this direction.
            self.stop_auto_scroll();
            return;
        }
        // Keep the focus content row tracking the moved content (the right
        // panel's contract — the band follows the scrolled content).
        self.selection_focus_content_y = self.selection_focus_content_y.saturating_add(int_scroll);
    }

    pub fn stop_auto_scroll(&mut self) {
        if self.is_auto_scrolling {
            self.is_auto_scrolling = false;
            self.auto_scroll_accumulator = 0.0;
            self.auto_scroll_speed = 0.0;
        }
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

    /// Whether any pane is still rehydrating its replayed history.
    #[must_use]
    pub fn any_rehydrating(&self) -> bool {
        self.panes.iter().any(|p| p.rehydrating)
    }

    /// Apply one sub-agent event to pane `pane`'s activity stream. Out-of
    /// range panes are ignored: every pane incl. daemon is ordinary here.
    pub fn apply_event(&mut self, pane: usize, event: &SubagentEvent) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.activity.apply(event);
        }
    }

    /// Deliver the turn's terminal report to pane `pane` — the `AgentsPaneDone`
    /// `output`, which is the SAME text the stream already played into the
    /// pane's timeline (the `TurnClosure` contract: the report is the last
    /// streamed message segment). Appending it as another `Message` event
    /// would duplicate the answer: the coalescer's backscan merges the
    /// marker into the existing Message entry and the pane reads
    /// `...hoje?Tudo bem! Como posso ajudar hoje?`.
    ///
    /// The completion path instead applies the SAME deduplication the right
    /// panel's seal uses (`SubagentActivity::seal`): the last Message entry
    /// whose text is contained in the report is DROPPED (the report below
    /// carries it), then the marker — the raw report on success, the
    /// `[turn <reason>]` prefix on failure — is appended as the turn's
    /// closing block. A report that does NOT contain the streamed tail (a
    /// turn that ended right after a tool call and fell back to a previous
    /// segment, a truncated report) keeps the entry: degrades to keeping,
    /// never to losing content.
    pub fn finish_turn(&mut self, pane: usize, output: &str, stop_reason: &str) {
        let Some(p) = self.panes.get_mut(pane) else {
            return;
        };
        let failed = stop_reason == "error" || stop_reason == "cancelled";
        p.activity.seal(!failed, Some(output));
        let marker = if failed {
            format!("\n\n[turn {stop_reason}] {output}")
        } else {
            output.to_string()
        };
        p.activity
            .apply(&SubagentEvent::Message { text: marker });
    }

    /// The list label of pane `pane` in the right panel's mirror entry:
    /// `#k` (1-based occurrence) when THIS split runs the same agent CLI in
    /// several panes — two `claude` instances must stay distinguishable in
    /// the shared subagent list — and `None` for a lone instance (the agent
    /// name alone is unambiguous there).
    #[must_use]
    pub fn pane_mirror_label(&self, pane: usize) -> Option<String> {
        let agent = self.panes.get(pane)?.agent.as_str();
        let same: Vec<usize> = self
            .panes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.agent == agent)
            .map(|(i, _)| i)
            .collect();
        if same.len() <= 1 {
            return None;
        }
        let k = same.iter().position(|&i| i == pane)? + 1;
        Some(format!("#{k}"))
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
/// Heuristic (documented at module level; NOT a perfect bin-packing): EVERY
/// pane — the cosh daemon included — is an equal participant in the tiling,
/// so the daemon's block comes out of the same area division as the agents'
/// instead of a fixed strip:
/// - 1 pane: the whole area.
/// - `n >= 2`: a single row of equal columns while every column stays >=
///   [`MIN_COLUMN_WIDTH`] wide (with 1-column gaps); otherwise two balanced
///   rows — `ceil(n/2)` panes on top, `floor(n/2)` below, and when the
///   available height is odd the TOP row takes the extra row.
/// - Gaps are exactly 1 cell (row/column) between panes; there are no
///   double borders.
///
/// Pane ORDER is preserved: pane 0 (the daemon) keeps its index position in
/// the grid, so focus hotkeys and dispatch routing stay stable.
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

    // Every pane is an equal participant: the single-row test uses the FULL
    // pane count (the daemon is not subtracted), so its block is one of the
    // equal cells rather than a special-cased strip.
    let single_row = {
        let usable = area.width.saturating_sub((pane_count - 1) as u16 * GAP);
        usable / pane_count as u16 >= MIN_COLUMN_WIDTH
    };

    if single_row {
        split_columns(area.x, area.y, area.width, area.height, pane_count)
    } else {
        let top_count = pane_count.div_ceil(2);
        let bottom_count = pane_count - top_count;
        // Balanced rows; the top row takes the extra row of an odd height.
        // Below 3 rows the separator cannot fit — split what is left.
        let (top_h, bottom_h, row_gap) = if area.height >= 2 * GAP + 2 {
            let usable = area.height - GAP;
            let top = usable.div_ceil(2);
            (top, usable - top, GAP)
        } else {
            let top = area.height.min(1);
            (top, area.height - top, 0)
        };
        let mut out = split_columns(area.x, area.y, area.width, top_h, top_count);
        out.extend(split_columns(
            area.x,
            area.y + top_h + row_gap,
            area.width,
            bottom_h,
            bottom_count,
        ));
        out
    }
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

/// Paint one pane: element background, the agent-name header and
/// the tail of the activity stream (or the idle hint) — rendered through
/// the right panel's REAL pipeline (`activity_lines` + `SubagentBlockRenderer`),
/// so markdown blocks wrap, style and cache exactly like the sub-agent
/// mini-chat instead of a stripped-down mirror.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    buf: &mut Buffer,
    rect: Rect,
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
    // Background fill: a whisper lighter than the base background — just
    // enough to separate the pane from the transcript without a visible
    // contrast edge. background (#07070A) → #0A0A0D.
    let bg = Style::default().bg(rgba_color(RGBA::from_hex("#0A0A0D")));
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(bg);
            }
        }
    }

    // ── Header row: the agent's name, one breathing column in ──
    let name_x = rect.x.saturating_add(1);
    let name_w = rect.width.saturating_sub(1);
    // The BRAND casing ("OpenCode"), not the lowercase ACP id — the id
    // stays the dispatch key; only the painted label changes. The palette
    // lookup is case-insensitive, so the brand colors still resolve.
    let label = display_name(&pane.agent).to_string();
    draw_agent_name(buf, &label, name_x, rect.y, name_w, theme, focused);
    if pane.rehydrating {
        // The cold-restore state: the pane's bound history is streaming
        // back from the agent's `session/load` replay. Just the simple
        // braille spinner (`component::spinner`), one breathing column in
        // from the pane's right edge — no label, no extra chrome.
        pane.rehydrate_spinner.advance();
        if let Some(cell) = buf.cell_mut((rect.right() - 1, rect.y)) {
            cell.set_char(pane.rehydrate_spinner.current_char());
            cell.set_style(Style::default().fg(rgba_color(theme.text_muted)));
        }
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
        // Empty pane: the background fill painted above is the whole body —
        // no placeholder text (visual pollution). The zero-content
        // measurement still lands so a stale scroll offset cannot survive.
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
    // The follow branch also WRITES BACK into `scroll_y`: without this the
    // pane stays at 0 while it follows, and the next `scroll_by` would clamp
    // against a stale origin (scrolling would visibly do nothing).
    let scroll_y = if pane.auto_follow {
        max_scroll
    } else {
        pane.scroll_y.min(max_scroll)
    };
    pane.scroll_y = scroll_y;
    pane.effective_scroll = scroll_y;
    pane.content_height = content_height;
    pane.body_height = body_h;

    // Sliding window: walk the lines with their absolute content row, skip
    // the ones fully above `scroll_y`, paint the ones intersecting the body
    // (passing `skip` so a straddling block shows only its lower part) and
    // label every painted visual row with its CONTENT row — the selection
    // regions' coordinate system, exactly the right panel's contract.
    let theme_key = subagent_theme_key(theme);
    // Same whisper-lighter background for markdown blocks so they merge
    // with the pane instead of floating as distinct boxes.
    let tint = RGBA::from_hex("#0A0A0D");
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
        SubagentActivityLine::Message { text, .. }
        | SubagentActivityLine::UserMessage { text, .. }
        | SubagentActivityLine::Thought { text, .. } => {
            // Persistent chat-style renderer per (pane, timeline) block —
            // `set_content` re-parses only the streamed tail. Thought is
            // dimmed, exactly like the right panel's mini-chat; the
            // replayed user block renders at full text color, exactly
            // like the agent's message (no invented marking).
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

/// The pane header's agent name: bold, painted with the agent's OWN logo
/// palette (`subagent::palette`) — gradient marks (Gemini's sparkle,
/// Devin's tri-color) sweep their colors across the glyphs, single-color
/// marks (Claude's terracotta, Kilo's acid yellow) paint solid. `cosh` and
/// agents outside the catalog keep the theme's text colors: no invented
/// brand for the TUI's own pane. An unfocused name dims toward the element
/// background so the focused pane reads at a glance, and a brand color too
/// close to the pane background lifts toward the readable contrast color
/// instead of dissolving into it.
fn draw_agent_name(
    buf: &mut Buffer,
    agent: &str,
    x: u16,
    y: u16,
    max_w: u16,
    theme: &Theme,
    focused: bool,
) {
    let chars: Vec<char> = agent.chars().filter(|c| !c.is_control()).collect();
    let count = chars.len();
    for (i, ch) in chars.into_iter().enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        let Some(cx) = x.checked_add(i) else { break };
        if cx >= x.saturating_add(max_w) {
            break;
        }
        // One sample per glyph column: `t` walks the mark's colors left to
        // right (constant for single-color marks).
        let t = if count <= 1 {
            0.0
        } else {
            f32::from(i) / (count as f32 - 1.0)
        };
        let style = brand_fg(agent, t, theme, focused).add_modifier(Modifier::BOLD);
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// The style of ONE glyph column of an agent's brand name, shared by the
/// split-pane headers and the right panel's subagent list (the bound
/// `/agents` agents get the same full-color logo there; normal subagents
/// pass `None`'s palette branch and stay in the theme's own colors —
/// opaque, never a half-brand tint).
///
/// A brand mark never dissolves into the surface: when the sampled color's
/// luminance sits too close to the background's, it lifts toward the
/// readable contrast color instead.
pub(crate) fn brand_fg(agent: &str, t: f32, theme: &Theme, focused: bool) -> Style {
    let bg_lum = {
        let (r, g, b, _) = theme.background_element.to_ints();
        luminance(r, g, b)
    };
    match palette_for(agent) {
        Some(palette) => {
            let rgb = palette.sample(t);
            let mut brand = RGBA::from_ints(rgb.r, rgb.g, rgb.b, 255);
            let (br, bg_, bb, _) = brand.to_ints();
            if (luminance(br, bg_, bb) - bg_lum).abs() < BRAND_CONTRAST_FLOOR {
                brand = blend(brand, contrast_fg(theme.background_element), 0.6);
            }
            let fg = if focused {
                brand
            } else {
                blend(theme.background_element, brand, DIM_UNFOCUSED)
            };
            Style::default().fg(rgba_color(fg))
        }
        None => {
            let fg = if focused {
                theme.text
            } else {
                theme.text_muted
            };
            Style::default().fg(rgba_color(fg))
        }
    }
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
            // Order preservation: the daemon keeps its grid slot (dispatch
            // routing and hotkeys depend on the stable index).
            assert_eq!(daemon.y, area.y, "pane 0 first in the grid");
            assert_eq!(daemon.x, area.x);
            assert!(daemon.area() < area.area(), "daemon is not the whole area");
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
        let sum: u32 = rects.iter().map(|r| r.area()).sum();
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
    fn two_panes_split_into_equal_cells() {
        let area = Rect::new(0, 0, 80, 24);
        let rects = compute_layout(2, area);
        // BOTH panes are equal participants: equal height, side by side.
        assert_eq!(rects[0].height, rects[1].height, "equal row heights");
        assert_eq!(
            rects[0].width + rects[1].width + GAP,
            80,
            "columns tile with one gap"
        );
        assert_eq!(rects[1].right(), area.right(), "agent reaches the edge");
    }

    #[test]
    fn narrow_area_falls_back_to_two_rows() {
        // 7 panes at 60 cols: per-column width would be ~9 < 24 → 2 rows.
        let area = Rect::new(0, 0, 60, 20);
        let rects = compute_layout(7, area);
        let row_tops: Vec<u16> = rects.iter().map(|r| r.y).collect();
        let distinct: Vec<u16> = {
            let mut v = row_tops.clone();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(distinct.len(), 2, "two pane rows, tops {row_tops:?}");
        // ceil(7/2) = 4 panes on the top row, floor(7/2) = 3 on the bottom.
        assert_eq!(
            row_tops.iter().filter(|&&y| y == distinct[0]).count(),
            4,
            "top row = ceil(n/2)"
        );
        assert_eq!(
            row_tops.iter().filter(|&&y| y == distinct[1]).count(),
            3,
            "bottom row = floor(n/2)"
        );
    }

    #[test]
    fn new_keeps_picker_order_and_never_injects_the_daemon() {
        // Used to insert a forced `cosh` pane 0 — the Cosh Manager block
        // then existed in /agents without the user ever picking cosh. The
        // split must be a plain reflection of the picker's marks.
        let state = AgentsSplitState::new(vec!["kilo".into(), "claude".into()]);
        assert_eq!(state.pane_count(), 2, "no daemon pane appears uninvited");
        assert_eq!(state.panes[0].agent, "kilo");
        assert_eq!(state.panes[1].agent, "claude");
        assert_eq!(state.drafts.len(), 2, "drafts aligned with panes");

        // An explicitly marked cosh keeps its marked position (first) and is
        // NOT duplicated.
        let state = AgentsSplitState::new(vec![DAEMON_LABEL.into(), "claude".into()]);
        assert_eq!(state.pane_count(), 2);
        assert_eq!(state.panes[0].agent, "cosh");
        assert_eq!(state.panes[1].agent, "claude");
    }

    #[test]
    fn focus_hotkey_targets_panes_by_position() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        // '1' targets pane 1 (kilo) — hotkeys index the pane LIST directly,
        // no phantom daemon slot in between.
        assert!(state.focus_hotkey('1'));
        assert_eq!(state.focused, 1);
        assert_eq!(state.focused_pane().map(|p| p.agent.as_str()), Some("kilo"));
        // '0' is the FIRST pane — the daemon only when the user marked it.
        assert!(state.focus_hotkey('0'));
        assert_eq!(state.focused, 0);
        assert_eq!(state.focused_pane().map(|p| p.agent.as_str()), Some("claude"));
        // Out-of-range and unknown keys leave the focus alone.
        assert!(!state.focus_hotkey('2'));
        assert_eq!(state.focused, 0);
        assert!(!state.focus_hotkey('9'));
        assert_eq!(state.focused, 0);
        assert!(!state.focus_hotkey('x'));
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn focus_next_prev_wrap_around() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        assert_eq!(state.pane_count(), 2);
        // Focused starts on pane 0.
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
            0,
            &SubagentEvent::Message {
                text: "hello ".to_string(),
            },
        );
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "world".to_string(),
            },
        );
        state.apply_event(
            7,
            &SubagentEvent::Message {
                text: "lost pane".to_string(),
            },
        );

        // The neighbour pane (1) stays empty; pane 0 coalesced its two
        // chunks and the out-of-range event was dropped.
        assert!(state.panes[1].activity.timeline.is_empty());
        assert_eq!(state.panes[0].activity.timeline.len(), 1);
        let SubagentTimelineEntry::Message { text } = &state.panes[0].activity.timeline[0] else {
            panic!("expected a Message entry");
        };
        assert!(text.contains("hello") && text.contains("world"), "got {text:?}");
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

    /// Regression: the terminal report must NOT duplicate the streamed
    /// answer. `AgentsPaneDone.output` is the SAME text the stream already
    /// played into the pane (the `TurnClosure` contract — the report is the
    /// last streamed message segment); delivering it as another `Message`
    /// event let the coalescer's backscan merge it into that same entry and
    /// the pane read "…hoje?Tudo bem! Como posso ajudar hoje?". `finish_turn`
    /// seals the streamed entry away (the same dedup the right panel's seal
    /// applies) before appending the report, and a failure report keeps the
    /// streamed narration (its body is the harness error, not the answer).
    #[test]
    fn finish_turn_does_not_duplicate_the_streamed_answer() {
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        // The live stream: chunks coalesce into ONE Message entry.
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "Tudo bem! Como\n".to_string(),
            },
        );
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "posso ajudar hoje?".to_string(),
            },
        );
        assert_eq!(state.panes[0].activity.timeline.len(), 1);

        // The turn ends: the report IS that same text.
        state.finish_turn(0, "Tudo bem! Como\nposso ajudar hoje?", "end_turn");

        // The streamed entry was dropped (the report carries it) and the
        // report landed ONCE — the timeline never contains a doubled
        // answer.
        let doubled = state
            .panes[0]
            .activity
            .timeline
            .iter()
            .filter_map(|e| match e {
                SubagentTimelineEntry::Message { text } => Some(text.clone()),
                _ => None,
            })
            .any(|text| {
                let count = text
                    .match_indices("Como")
                    .count();
                count > 1
            });
        assert!(
            !doubled,
            "the answer must appear exactly once: {timeline:?}",
            timeline = state.panes[0].activity.timeline
        );
        // The report is present as the closing block.
        let last = state.panes[0].activity.timeline.last().cloned().unwrap();
        let SubagentTimelineEntry::Message { text } = last else {
            panic!("expected the report block");
        };
        assert!(
            text.contains("posso ajudar hoje?"),
            "the report landed in the pane: {text:?}"
        );

        // The failure path KEEPS the streamed narration (the body is the
        // harness error, so nothing is deduplicated away).
        let mut failed = AgentsSplitState::new(vec!["kilo".into()]);
        failed.apply_event(
            0,
            &SubagentEvent::Message {
                text: "partial answer".to_string(),
            },
        );
        failed.finish_turn(0, "spawn failed", "error");
        let texts: Vec<String> = failed.panes[0]
            .activity
            .timeline
            .iter()
            .filter_map(|e| match e {
                SubagentTimelineEntry::Message { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("partial answer")),
            "failure keeps the streamed narration: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("[turn error]")),
            "the failure marker is labeled: {texts:?}"
        );
    }

    #[test]
    fn any_running_tracks_the_running_flag() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        assert!(!state.any_running());
        state.set_running(1, true);
        assert!(state.any_running());
        state.set_running(1, false);
        assert!(!state.any_running());
        // Out-of-range set_running is a no-op.
        state.set_running(9, true);
        assert!(!state.any_running());
    }

    /// A rehydrating pane's header paints ONLY the simple braille spinner
    /// (`component::spinner`) at the pane's right edge — no label, no extra
    /// chrome — and consecutive renders advance the frame. A
    /// non-rehydrating pane paints nothing.
    #[test]
    fn render_rehydrating_header_spins_alone() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        state.panes[0].rehydrating = true;
        let area = Rect::new(0, 0, 80, 24);
        let flat_text = |buf: &Buffer| {
            (0..24)
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
                .join("\n")
        };

        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let all: String = flat_text(&buf);
        // The first frame's glyph sits in pane 0's header row.
        assert!(
            all.contains(crate::component::spinner::SPINNER_FRAMES[0]),
            "the spinner glyph paints in the header: {all:?}"
        );
        // Nothing else was invented: no label, no status dot.
        assert!(!all.contains("restoring"), "no label: {all:?}");
        assert!(!all.contains('●') && !all.contains('○'), "no status dots: {all:?}");

        // Two further renders consume the spinner's half-rate tick
        // (`advance` moves the frame every other tick): the glyph changes.
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let all2 = flat_text(&buf);
        assert!(
            !all2.contains(crate::component::spinner::SPINNER_FRAMES[0]),
            "the frame advanced past glyph 0: {all2:?}"
        );

        // A settled pane paints no spinner at all.
        state.panes[0].rehydrating = false;
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let all3 = flat_text(&buf);
        assert!(
            !all3.contains(crate::component::spinner::SPINNER_FRAMES[0]),
            "no spinner when settled: {all3:?}"
        );
    }

    #[test]
    fn render_paints_pane_headers_without_a_placeholder() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        assert_eq!(state.pane_rects.len(), 2, "hit rects recorded for mouse");

        let row_text = |y: u16| {
            (0..80)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect::<String>()
        };
        let all: String = (0..24).map(row_text).collect::<Vec<_>>().join("\n");
        assert!(all.contains("claude"), "agent header: {all:?}");
        assert!(all.contains("kilo"), "agent header: {all:?}");
        assert!(!all.contains('['), "no index labels: {all:?}");
        assert!(
            !all.contains('●') && !all.contains('○'),
            "no status dots: {all:?}"
        );
    }

    /// The header name sits one breathing column past the pane edge, is
    /// bold, and carries the agent's OWN brand color: Claude focused paints
    /// the terracotta (lifted only when the theme would dissolve it), the
    /// cosh daemon keeps the theme's text color (no invented brand), and an
    /// unfocused brand name dims toward the background.
    #[test]
    fn header_name_is_indented_bold_and_brand_colored() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let claude_rect = state.pane_rects[0];
        let header_y = claude_rect.y;
        let glyph_at = |x: u16| buf.cell((x, header_y)).map(|c| c.symbol().to_string());

        // One column of breathing room, then the name.
        assert_eq!(glyph_at(claude_rect.x).as_deref(), Some(" "));
        assert_eq!(glyph_at(claude_rect.x + 1).as_deref(), Some("c"));

        // Bold across the whole name.
        for (i, ch) in "claude".chars().enumerate() {
            let cell = buf
                .cell((claude_rect.x + 1 + i as u16, header_y))
                .unwrap_or_else(|| panic!("cell {i}"));
            assert_eq!(cell.symbol(), ch.to_string());
            assert!(
                cell.modifier.contains(Modifier::BOLD),
                "name glyph {ch} is bold"
            );
        }

        // Focus kilo: claude's name dims toward the background (the focus
        // highlight belongs to whoever owns it), or its contrast-lifted
        // variant on a theme where the raw color would dissolve — the same
        // formula the painter applies.
        state.focus_next();
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let brand = RGBA::from_ints(0xD9, 0x77, 0x57, 255);
        let bg_lum = {
            let (r, g, b, _) = theme.background_element.to_ints();
            luminance(r, g, b)
        };
        let (br, bg_, bb, _) = brand.to_ints();
        let expected = if (luminance(br, bg_, bb) - bg_lum).abs() < BRAND_CONTRAST_FLOOR {
            blend(brand, contrast_fg(theme.background_element), 0.6)
        } else {
            brand
        };
        let cell = buf
            .cell((claude_rect.x + 1, header_y))
            .expect("claude 'c' cell");
        assert_ne!(
            cell.style().fg,
            Some(rgba_color(theme.text_muted)),
            "claude keeps a brand-derived color, never the naked muted tone"
        );
        if expected != brand {
            assert_eq!(
                cell.style().fg,
                Some(rgba_color(expected)),
                "a contrast-threatened brand is lifted"
            );
        }

        // Focus back to claude: the name takes the brand terracotta.
        state.focus_hotkey('0');
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let cell = buf
            .cell((claude_rect.x + 1, header_y))
            .expect("claude 'c' cell");
        assert_eq!(
            cell.style().fg,
            Some(rgba_color(expected)),
            "focused claude paints the brand color"
        );

        // Unfocused kilo dims toward the background, NOT pure brand.
        let kilo_rect = state.pane_rects[1];
        let cell = buf.cell((kilo_rect.x + 1, kilo_rect.y)).unwrap();
        assert_ne!(
            cell.style().fg,
            Some(rgba_color(expected)),
            "unfocused is dimmed"
        );
    }

    /// A multi-stop gradient mark sweeps its colors across the glyphs: the
    /// first and last glyph take the mark's endpoint colors (post
    /// contrast-adjustment) and an interior glyph differs from both. This
    /// pins the `t = i / (count - 1)` walk in `draw_agent_name` — the path
    /// `palette.sample` only exercises indirectly elsewhere.
    #[test]
    fn gradient_mark_sweeps_across_the_glyphs() {
        let theme = test_theme();
        // "devin" carries the official tri-color logomark (blue → green →
        // cyan-blue), so the name paints as a real sweep.
        let mut state = AgentsSplitState::new(vec!["kilo".into(), "devin".into()]);
        state.focus_next();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let rect = state.pane_rects[1];
        let header_y = rect.y;
        let name = "devin";
        let fgs: Vec<_> = (0..name.len())
            .map(|i| {
                buf.cell((rect.x + 1 + i as u16, header_y))
                    .map(|c| c.style().fg)
                    .unwrap()
            })
            .collect();

        // The sweep really moves: not every glyph is the same color.
        assert!(
            fgs.iter().any(|fg| *fg != fgs[0]),
            "gradient glyphs must vary: {fgs:?}"
        );
        // The walk starts at the palette's first stop — the raw primary
        // (or its contrast-lifted variant, the same formula the painter
        // applies; keep this assertion in sync with `draw_agent_name`).
        let palette = palette_for("devin").expect("devin palette");
        let brand = RGBA::from_ints(
            palette.primary.r,
            palette.primary.g,
            palette.primary.b,
            255,
        );
        let bg_lum = {
            let (r, g, b, _) = theme.background_element.to_ints();
            luminance(r, g, b)
        };
        let (br, bg_, bb, _) = brand.to_ints();
        let expected = if (luminance(br, bg_, bb) - bg_lum).abs() < BRAND_CONTRAST_FLOOR {
            blend(brand, contrast_fg(theme.background_element), 0.6)
        } else {
            brand
        };
        assert_eq!(fgs[0], Some(rgba_color(expected)), "first glyph = primary");
    }

    #[test]
    fn render_running_pane_shows_the_activity_tail() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["claude".into()]);
        state.set_running(0, true);
        // More lines than the pane body can hold, so the tail scroll is
        // actually exercised. The grid layout gives the single picked pane
        // the whole session area (~23 rows of body), so the volume is sized
        // against the measured content height, not a hardcoded strip.
        for i in 0..40 {
            state.apply_event(
                0,
                &SubagentEvent::Message {
                    text: format!("line {i} of the stream\n"),
                },
            );
        }
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        // The effective window content rows the render measured.
        let pane = &state.panes[0];
        assert!(
            pane.content_height > pane.body_height,
            "test needs overflow: content={} body={}",
            pane.content_height,
            pane.body_height
        );
        let max_scroll = pane.content_height - pane.body_height;

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
        // Tail scroll: the window top sits at content row `max_scroll`, so
        // everything strictly above (up to `max_scroll - 1`) is off-screen
        // and the first visible line is exactly `max_scroll`.
        assert!(!all.contains("line 0 "), "old lines scrolled off: {all:?}");
        assert!(
            !all.contains(&format!("line {} ", max_scroll.saturating_sub(1))),
            "everything below the window top is off-screen: {all:?}"
        );
        assert!(
            all.contains(&format!("line {max_scroll} ")),
            "the window top row is visible: {all:?}"
        );
        assert!(
            all.contains("line 39 of the stream"),
            "newest line visible: {all:?}"
        );
    }
}
