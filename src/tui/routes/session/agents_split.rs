//! Split-view "panes" for the `/agents` route: one pane per agent the user
//! picked (the `cosh` daemon pane only when explicitly marked), laid out
//! over the session area with a simple bin-packing heuristic.
//!
//! This module owns ONLY the pane model + layout + painting. The prompt is
//! a single shared input that lives outside this area (`compute_prompt_area`
//! in `app/render.rs`), so routing a submission is not this module's
//! concern — it only tracks which pane (if any) the prompt targets.
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
use std::time::{Duration, Instant};

use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::MarkdownRenderable;
use cosh_tools::subagent::events::{
    AgentCommand, AgentConfigOption, PlanEntryStatus, SubagentEvent, ToolCallStatus,
};
use cosh_tools::subagent::palette::{display_name, palette_for};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::component::budget::{budget_bar_style, budget_block, budget_pct_style};
use crate::component::prompt_message_box;
use crate::component::spinner::SpinnerState;
use crate::routes::session::right_panel::subagent_theme_key;
use crate::routes::session::right_panel::types::{
    SubagentDiffCells, SubagentTimelineEntry, activity_lines, subagent_diff_block,
    subagent_diff_content_key, subagent_diff_rows, SubagentActivity, SubagentActivityLine,
    SubagentBlockRenderer, INPUT_BOX_PAD_ROWS,
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

/// Hotkey of the MAIN agent: it unfocuses every pane, so the shared
/// prompt targets the normal-mode loop (running as manager while the
/// split is open). The panes take `1`..=`9` in picker order (1-based),
/// each reachable with a single keypress.
const UNFOCUS_HOTKEY: char = '0';

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

/// How long a pane may stay `running` without ANY event (stream, tool
/// update, terminal Done) before the frame-tick watchdog force-closes its
/// turn (issue 5): a turn whose thread died or whose provider connection
/// silently stalled otherwise pins the pane's spinner forever, the
/// dispatch gate refuses every new prompt, and nothing can recover it.
/// Generous on purpose — a long tool call (a big build, a slow ACP turn
/// with no chunk yet) stays far under it; the watchdog only fires on
/// turns that are genuinely gone.
pub const PANE_IDLE_WATCHDOG: Duration = Duration::from_secs(600);

/// One agent pane: identity, live activity stream and lifecycle state.
#[derive(Debug, Clone, Default)]
pub struct AgentPane {
    /// Agent CLI name (`"claude"`, `"kilo"`, …). The daemon pane is `"cosh"`.
    pub agent: String,    /// Structured live transcript, REUSED from the right panel's
    /// `SubagentActivity` so Message/Thought chunks coalesce and tool-call
    /// patches apply exactly like the existing sub-agent box.
    pub activity: SubagentActivity,
    /// Whether a sub-agent turn is currently running in this pane.
    pub running: bool,
    /// Last time this pane showed ANY sign of life while `running`: the
    /// dispatch itself (`set_running(true)`), a streamed event
    /// (`apply_event`) or the terminal report. `None` when idle. The
    /// frame-tick watchdog reads it (`stale_panes`): a pane `running`
    /// with no activity for [`PANE_IDLE_WATCHDOG`] is force-closed —
    /// its turn thread died or stalled with no way to report.
    pub last_activity: Option<Instant>,
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
    /// What serves this pane's session, from the ACP config options: the
    /// prompt badge's `(model, reasoning)` pair. Empty until the first
    /// turn (or replay pass) reports it; every report replaces it, so a
    /// model/thought change shows on the next turn.
    pub session_info: Option<AgentSessionInfo>,
    /// The agent's own slash commands, from `available_commands_update` (a
    /// live turn) or the on-demand snapshot probe. `None` = never fetched;
    /// `Some(vec![])` = fetched and the harness advertised none. Feeds the
    /// slash menu while THIS pane holds the focus (the agent's exact
    /// spellings, blocklist-filtered by the app layer).
    pub commands: Option<Vec<AgentCommand>>,
    /// The session's config options (model / provider / thought level /
    /// mode), same sources as `commands`. Feeds the standardized config
    /// boxes the mapped command groups open.
    pub config_options: Option<Vec<AgentConfigOption>>,
    /// Option ids the user actually picked in a config box (the box
    /// confirm records them). `dispatch_to_pane` sends ONLY these ids'
    /// currents as turn picks — an untouched option rides on the harness's
    /// own default, letting `acp::call`'s registry default (e.g. a working
    /// free model where the harness's default is a paid-tier login wall)
    /// still apply.
    pub picked_config_ids: Vec<String>,
    /// True while an on-demand snapshot probe for this pane is in flight
    /// (focus arrived with no cache): the slash menu shows its spinner row
    /// until the probe returns.
    pub snapshot_loading: bool,
    /// Top of the visible window in content rows — the chat's `scroll_y`.
    /// Re-clamped every render against the real content; sticky-bottom
    /// follow is [`AgentPane::auto_follow`].
    pub scroll_y: u16,
    /// The INPUT boxes (the user's echo, an `agents_call` input) of this
    /// pane's timeline, in timeline order — the chat's clickable
    /// user-message boxes' equivalent. Rebuilt every render (content
    /// coordinates, pad rows included); the hover highlight and the input
    /// click hit-test read it. Empty between renders.
    pub input_rows: Vec<InputBoxHit>,
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

/// The `/agents` prompt badge's data for one pane: what serves its
/// session, read from the ACP config options (never guessed). `model` is
/// the effective model id, `model_label` its human label when the harness
/// names the value, `provider` (+ `provider_label`) the serving provider
/// when advertised at all, `reasoning` the thought-level display when the
/// harness offers a real choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSessionInfo {
    pub model: Option<String>,
    pub model_label: Option<String>,
    pub provider: Option<String>,
    pub provider_label: Option<String>,
    pub reasoning: Option<String>,
}

impl AgentPane {
    #[must_use]
    pub fn new(agent: String) -> Self {
        Self {
            agent,
            activity: SubagentActivity::default(),
            running: false,
            last_activity: None,
            stop_signal: None,
            session_id: None,
            session_info: None,
            commands: None,
            config_options: None,
            picked_config_ids: Vec::new(),
            snapshot_loading: false,
            rehydrating: false,
            rehydrate_spinner: SpinnerState::new(),
            scroll_y: 0,
            input_rows: Vec::new(),
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

    /// Whether screen position `(x, y)` sits on one of this pane's input
    /// boxes (the render registry, LAST frame's layout): `true` for the
    /// pane + the box's content-row span. The click hit-test and the
    /// hover walker share this.
    pub fn input_box_at(&self, rect: Rect, x: u16, y: u16) -> Option<std::ops::Range<i32>> {
        self.input_hit_at(rect, x, y).map(|hit| hit.span.clone())
    }

    /// The full hit of the input box under `(x, y)` (the render registry,
    /// LAST frame's layout): span + timeline index + user ordinal.
    pub fn input_hit_at(&self, rect: Rect, x: u16, y: u16) -> Option<&InputBoxHit> {
        if x < rect.x || x >= rect.x + rect.width || y < rect.y + 1 || y >= rect.bottom() {
            return None;
        }
        let row = self.content_row_at(rect, y);
        self.input_rows.iter().find(|hit| hit.span.contains(&row))
    }
}

/// State of the whole `/agents` split view.
#[derive(Debug, Default)]
pub struct AgentsSplitState {
    /// One pane per agent in picker order. The `cosh` daemon pane exists
    /// only when the picker marked it, and only as an ordinary pane.
    pub panes: Vec<AgentPane>,
    /// The pane under the cursor AND the content-row span of the input
    /// box hovered there (the chat's `hovered_msg_idx` contract, pane
    /// scoped): the render lights the box's rows with
    /// `background_element`, the click-ability cue. `None` = no pane
    /// input hovered. Cleared by any move off the pane/box.
    pub hovered_input: Option<(usize, InputBoxHit)>,
    /// A confirmed plain click ON a pane's input box: `(pane, hit)` —
    /// the pane plus the clicked box's identity (timeline index for
    /// Copy, user ordinal for Revert/Fork). The app layer resolves it to
    /// the pane session's message and opens the Message Actions dialog
    /// (the chat's pending_message_action contract, pane scoped).
    /// Consumed by the mouse dispatcher.
    pub pending_input_action: Option<(usize, InputBoxHit)>,
    /// Focused pane index. `None` means NO pane holds the focus: the
    /// shared prompt targets the MAIN agent (the same loop as the normal
    /// mode, running as manager while the split is open). `Ctrl+0`
    /// unfocuses; a pane hotkey or click focuses again. Fresh splits
    /// start on the first picked pane.
    pub focused: Option<usize>,
    /// The pane that took the whole session area (`Ctrl+O` toggle on the
    /// focused pane, or a double-click on it). The OTHER panes stay fully
    /// alive — turns keep running, streams keep landing in their
    /// timelines — they are simply painted nothing: the layout hands
    /// their slots degenerate rects (the already-supported zero-size
    /// path), so every index-keyed structure (pane_regions, block_cache,
    /// diff_cells, pane_rects) survives unchanged. `None` = the normal
    /// grid. A pane hotkey or double-click restores the grid.
    pub maximized: Option<usize>,
    /// Screen rects of the LAST rendered frame — the mouse dispatcher
    /// hit-tests these, exactly like `SessionView::pill_area`.
    pub pane_rects: Vec<Rect>,
    /// Persistent chat-style markdown renderers for the panes' Message/
    /// Thought blocks — the SAME cache pattern the right panel's sub-agent
    /// window uses (`SubagentBlockRenderer` survives across frames so
    /// `set_content` re-parses only the streamed tail). Key:
    /// `{pane_index}:{timeline_index}`. Lives on the split state (not the
    /// pane) so the pane model stays plain data.
    pub block_cache: HashMap<String, SubagentBlockRenderer>,
    /// Cached styled cells of the panes' `EditDiff` activity lines — the
    /// SAME per-line cache the right panel's window uses (`SubagentDiffCells`,
    /// pruned by the frame's live keys). Key: `{pane_index}:diff{index}`.
    /// Lives on the split state (not the pane) so the pane model stays
    /// plain data.
    pub diff_cells: HashMap<String, SubagentDiffCells>,
    /// Copy-by-selection state (the chat's drag contract, one selection for
    /// the whole split): the pane it started in, the screen-space anchor/
    /// focus, and the content rows they map to.
    pub selection_pane: Option<usize>,
    pub drag_selection: Option<(u16, u16, u16, u16)>,
    pub selection_anchor_content_y: i32,
    pub selection_focus_content_y: i32,
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
    /// resumes its ACP session).
    #[must_use]
    pub fn new(agents: Vec<crate::types::AgentPaneRef>) -> Self {
        let panes: Vec<AgentPane> = agents
            .iter()
            .filter(|r| !r.agent().is_empty())
            .map(AgentPane::from_ref)
            .collect();
        Self {
            panes,
            focused: Some(0),
            hovered_input: None,
            pending_input_action: None,
            maximized: None,
            pane_rects: Vec::new(),
            block_cache: HashMap::new(),
            diff_cells: HashMap::new(),
            selection_pane: None,
            drag_selection: None,
            selection_anchor_content_y: 0,
            selection_focus_content_y: 0,
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

    /// Focus the pane bound to hotkey `ch`: `'0'` UNFOCUSES every pane
    /// (the shared prompt then targets the MAIN agent — the normal-mode
    /// loop, running as manager while the split is open); `'1'..='9'`
    /// focus the 1st..9th pane in picker order. Returns the pane index the
    /// key FOCUSED (`None` for an unfocus, an out-of-range digit, or an
    /// unmatched key — the caller only needs the index to seed a
    /// per-pane fetch like the command snapshot).
    pub fn focus_hotkey(&mut self, ch: char) -> Option<usize> {
        // Jumping to another pane ends the expansion: the grid returns.
        self.maximized = None;
        if ch == UNFOCUS_HOTKEY {
            self.focused = None;
            return None;
        }
        let index = match ch {
            '1'..='9' => usize::from(ch as u8 - b'1'),
            _ => return None,
        };
        if index < self.panes.len() {
            self.focused = Some(index);
            Some(index)
        } else {
            None
        }
    }

    /// Toggle the expanded pane: an unfocused split maximizes the FOCUSED
    /// pane (focusing it first — expansion needs a target), an already-
    /// maximized split restores the grid. Returns whether the toggle
    /// HANDLED the key (the caller consumes it). The hotkeys stay
    /// consistent:
    /// `focus_hotkey` first RESTORES the grid (jumping to another pane
    /// ends the expansion — one click/keystroke back), then focuses.
    pub fn toggle_maximized(&mut self) -> bool {
        match self.maximized {
            // Restore: the grid returns, the key is consumed — the caller
            // must NOT park on the same press (the walk-back is two Escs).
            Some(_) => {
                self.maximized = None;
                true
            }
            None => {
                if let Some(focused) = self.focused {
                    self.maximized = Some(focused);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// The ESC walk-back primitive: restore the grid ONLY when a pane is
    /// expanded. `true` means the press was consumed (the caller must NOT
    /// park — the next Esc will); `false` means the grid was already up
    /// (nothing expanded) and the caller falls through to its own Esc
    /// semantics — the park. Distinct from [`Self::toggle_maximized`]
    /// because an Esc with the grid up must NEVER re-expand (a toggle
    /// would oscillate expand/restore forever and the user could never
    /// leave the split with Esc).
    pub fn restore_maximized(&mut self) -> bool {
        match self.maximized.take() {
            Some(_) => true,
            None => false,
        }
    }

    /// The currently focused pane, if any (`None` when the main agent
    /// holds the focus).
    #[must_use]
    /// Track the cursor over the split: the pane under `(x, y)` AND the
    /// input box its row falls on (the chat's `update_hover`, pane
    /// scoped). Returns `true` when the hover target changed (the caller
    /// repaints). Off-pane or off-box movement clears.
    pub fn update_hover(&mut self, x: u16, y: u16) -> bool {
        let next = self
            .pane_rects
            .iter()
            .position(|rect| {
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
            })
            .and_then(|i| {
                let rect = self.pane_rects[i];
                self.panes
                    .get(i)
                    .and_then(|pane| pane.input_hit_at(rect, x, y))
                    .cloned()
                    .map(|hit| (i, hit))
            });
        let changed = next != self.hovered_input;
        self.hovered_input = next;
        changed
    }

    /// A plain (non-drag) click on a pane's input box: resolve the pane +
    /// the input entry's timeline index and record it for the app layer
    /// (the Message Actions dialog). `false` when the click is not on an
    /// input box (the caller's normal pane handling proceeds).
    pub fn click_input_at(&mut self, x: u16, y: u16) -> bool {
        let target = self.pane_rects.iter().position(|rect| {
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
        });
        let Some(index) = target else {
            return false;
        };
        let rect = self.pane_rects[index];
        let hit = self
            .panes
            .get(index)
            .and_then(|pane| pane.input_hit_at(rect, x, y))
            .cloned();
        let Some(hit) = hit else {
            return false;
        };
        self.pending_input_action = Some((index, hit));
        true
    }

    /// Take the pending input action (consume-on-read, like the chat's
    /// `pending_message_action.take()`).
    pub fn take_input_action(&mut self) -> Option<(usize, InputBoxHit)> {
        self.pending_input_action.take()
    }

    /// Discard any pending input action (a drag consumed the press — the
    /// click never became a click).
    pub fn clear_input_action(&mut self) {
        self.pending_input_action = None;
    }

    /// The raw TEXT behind an input hit (the Copy action's source): the
    /// timeline entry's own text, un-sanitized — exactly what the sender
    /// typed/sent.
    pub fn input_text(&self, pane: usize, hit: &InputBoxHit) -> Option<String> {
        match self.panes.get(pane)?.activity.timeline.get(hit.timeline_index) {
            Some(SubagentTimelineEntry::UserMessage { text })
            | Some(SubagentTimelineEntry::AgentMessage { text }) => Some(text.clone()),
            _ => None,
        }
    }

    /// Replace a pane's whole timeline from replay events (the post-revert
    /// rebuild): clears the activity, then applies every event through the
    /// same coalescer a live turn streams through.
    pub fn reset_timeline(&mut self, pane: usize, events: &[SubagentEvent]) {
        let Some(p) = self.panes.get_mut(pane) else {
            return;
        };
        p.activity = SubagentActivity::default();
        for event in events {
            p.activity.apply(event);
        }
    }

    /// Clear the hovered pane-input highlight. `true` when one was set.
    pub fn clear_hover(&mut self) -> bool {
        if self.hovered_input.is_some() {
            self.hovered_input = None;
            return true;
        }
        false
    }

    pub fn focused_pane(&self) -> Option<&AgentPane> {
        self.focused.and_then(|i| self.panes.get(i))
    }

    /// Scroll the FOCUSED pane by `rows` (negative = up), the keyboard
    /// scroll path. Mirrors the chat's `scroll_by_raw` + accumulator reset
    /// cadence: each keypress moves whole rows against the LAST measured
    /// heights, no acceleration.
    pub fn scroll_focused_by(&mut self, rows: i32) {
        if let Some(i) = self.focused
            && let Some(pane) = self.panes.get_mut(i)
        {
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
    /// No-op while the main agent holds the focus.
    pub fn scroll_focused_to_top(&mut self) {
        if let Some(i) = self.focused
            && let Some(pane) = self.panes.get_mut(i)
        {
            pane.scroll_to_top();
        }
    }

    pub fn scroll_focused_to_bottom(&mut self) {
        if let Some(i) = self.focused
            && let Some(pane) = self.panes.get_mut(i)
        {
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
    /// anchor pane so a drag never leaks across panes). Both endpoints are
    /// content-space rows — the anchor stored at press time, the focus
    /// tracked live — so the selection survives scrolling during the drag,
    /// exactly like `SessionView::get_text_in_region`.
    #[must_use]
    pub fn extract_selected_text(&self) -> String {
        let Some(pane) = self.selection_pane else {
            return String::new();
        };
        let Some((sx, _, fx, _)) = self.drag_selection else {
            return String::new();
        };
        let Some(regions) = self.pane_regions.get(pane) else {
            return String::new();
        };
        let content_anchor = self.selection_anchor_content_y;
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

    /// Whether any pane has a running sub-agent turn.
    #[must_use]
    pub fn any_running(&self) -> bool {
        self.panes.iter().any(|p| p.running)
    }

    /// Indexes of panes whose turn is STALE for the watchdog (issue 5):
    /// `running` with no event of any kind for longer than
    /// [`PANE_IDLE_WATCHDOG`] — the turn thread died or stalled without a
    /// way to report. A pane whose `last_activity` is `None` while
    /// `running` (should not happen — the dispatch stamps it) counts as
    /// stale from the check, never from zero.
    #[must_use]
    pub fn stale_panes(&self, now: Instant) -> Vec<usize> {
        self.panes
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.running
                    && p.last_activity
                        .is_none_or(|t| now.duration_since(t) >= PANE_IDLE_WATCHDOG)
            })
            .map(|(i, _)| i)
            .collect()
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
            // Any sign of life pushes the watchdog back (issue 5): a
            // streaming turn is never stale, however long it runs.
            if p.running {
                p.last_activity = Some(Instant::now());
            }
            p.activity.apply(event);
        }
    }

    /// Echo the turn's input into pane `pane`'s timeline as a FRESH entry —
    /// the user's own prompt (`user = true`, the brand rail) or another
    /// agent's `agents_call` input (the distinct rail). Out-of-range panes
    /// are ignored, like [`Self::apply_event`].
    pub fn echo_turn_input(&mut self, pane: usize, text: &str, user: bool) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.activity.open_turn_input(text, user);
        }
    }

    /// Store the pane's serving pair from a `SessionModel` report,
    /// replacing whatever the previous turn reported. Out-of-range panes
    /// are ignored, like [`Self::apply_event`].
    pub fn set_session_info(&mut self, pane: usize, info: AgentSessionInfo) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.session_info = Some(info);
        }
    }

    /// Store the pane's slash commands from a `Commands` report (a live
    /// turn's `available_commands_update` or the snapshot probe's result),
    /// replacing any earlier list. Out-of-range panes are ignored.
    pub fn set_agent_commands(&mut self, pane: usize, commands: Vec<AgentCommand>) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.commands = Some(commands);
            p.snapshot_loading = false;
        }
    }

    /// Store the pane's config options from a `ConfigOptions` report,
    /// replacing any earlier list. Out-of-range panes are ignored.
    pub fn set_agent_config_options(&mut self, pane: usize, options: Vec<AgentConfigOption>) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.config_options = Some(options);
            p.snapshot_loading = false;
        }
    }

    /// Flag pane `pane`'s snapshot probe as in flight (focus arrived with
    /// an empty cache). Out-of-range panes are ignored.
    pub fn set_snapshot_loading(&mut self, pane: usize, loading: bool) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.snapshot_loading = loading;
        }
    }

    /// The focused pane's slash-menu source: `None` when NO pane is
    /// focused — OR the focused pane is the DAEMON (cosh itself: its
    /// commands are the TUI's own set, which the caller serves directly;
    /// an ACP probe would be meaningless). Otherwise the focused pane's
    /// entries: the group commands SYNTHESIZED from the pane's own ACP
    /// config options first (model / provider / mode — the controls
    /// harnesses like opencode and kilo never publish as slash
    /// commands, see `agent_command_map::synthesized_group_commands`),
    /// then the pane's own published command list blocklist-filtered, with
    /// any published command that duplicates a synthesized group name
    /// dropped (the group's box wins — one entry per control). Options
    /// still in flight contribute no synthesized entries yet — a later
    /// sync re-runs this method once they land.
    /// `snapshot_loading` marks an in-flight probe: the menu shows its
    /// spinner row until the entries land.
    #[must_use]
    pub fn slash_source(
        &self,
    ) -> Option<(Vec<crate::ui::slash_menu::SlashCommand>, bool)> {
        let pane = self.focused_pane()?;
        if pane.agent == DAEMON_LABEL {
            return None;
        }
        let agent = pane.agent.as_str();
        let mut entries: Vec<crate::ui::slash_menu::SlashCommand> = pane
            .config_options
            .as_ref()
            .map(|options| super::agent_command_map::synthesized_group_commands(options))
            .unwrap_or_default();
        // The synthesized group spellings, normalized once — the dedup
        // filter below reads this SET, not `entries` itself (the mutable
        // extend borrow would otherwise conflict with the closure's read).
        let group_names: std::collections::HashSet<String> = entries
            .iter()
            .map(|entry| super::agent_command_map::normalize(&entry.name))
            .collect();
        let commands = pane.commands.clone().unwrap_or_default();
        entries.extend(commands.iter().filter(|cmd| {
            !super::agent_command_map::is_blocked(agent, &cmd.name)
                // A published command that duplicates a synthesized group
                // (claude's `/model`, a future fixed opencode) is covered
                // by the synthesized entry: same box, standardized name.
                && !group_names
                    .contains(&super::agent_command_map::normalize(&cmd.name))
        }).map(|cmd| crate::ui::slash_menu::SlashCommand {
            name: cmd.name.clone(),
            // The agent's command NAME is verbatim; the description is
            // unwrapped to ONE line (harnesses publish whole skill
            // briefs — opencode's `customize-opencode`, claude's skill
            // commands — which flood the row otherwise).
            desc: super::agent_command_map::menu_description(&cmd.description),
        }));
        Some((entries, pane.snapshot_loading))
    }

    /// Deliver the turn's terminal report to pane `pane` — the `AgentsPaneDone`
    /// `output`. The closure itself lives in ONE place shared with the
    /// right-panel mirror ([`SubagentActivity::close_turn`]): seal the
    /// timeline, drop the streamed Message entry whose text the report
    /// carries (the `TurnClosure` contract — appending it raw would let the
    /// coalescer merge the marker into that same entry, the "answer twice"
    /// bug), then append the marker — the raw report on success, the
    /// `[turn <reason>]` prefix on failure — as the turn's closing block.
    /// A report that does NOT contain the streamed tail (a turn that ended
    /// right after a tool call and fell back to a previous segment, a
    /// truncated report) keeps the entry: degrades to keeping, never to
    /// losing content.
    pub fn finish_turn(&mut self, pane: usize, output: &str, stop_reason: &str) {
        let Some(p) = self.panes.get_mut(pane) else {
            return;
        };
        p.activity.close_turn(output, stop_reason);
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

    /// The registry id this pane is addressable under in `agents_call`: the
    /// agent CLI name, or `name#k` when THIS split runs several panes of the
    /// same CLI (the same disambiguation `pane_mirror_label` renders). The
    /// manager pane is addressable like every other agent: a `cosh` pane's
    /// id is `"cosh"` (or `"cosh#k"` among several `cosh` panes).
    #[must_use]
    pub fn pane_registry_id(&self, pane: usize) -> Option<String> {
        // Resolve the agent FIRST: this is also the range check
        // (`pane_mirror_label` returns `None` for a lone instance too, so it
        // cannot distinguish an out-of-range index).
        let agent = self.panes.get(pane)?.agent.clone();
        match self.pane_mirror_label(pane) {
            Some(suffix) => Some(format!("{agent}{suffix}")),
            None => Some(agent),
        }
    }

    /// Set the running flag of pane `pane` (out-of-range panes ignored).
    /// Starting a turn stamps the watchdog baseline: the pane is alive
    /// from dispatch, before its first event (issue 5).
    pub fn set_running(&mut self, pane: usize, running: bool) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.running = running;
            p.last_activity = running.then(Instant::now);
        }
    }

    /// Attach a cooperative stop flag to pane `pane`.
    pub fn set_stop_signal(&mut self, pane: usize, signal: Option<Arc<AtomicBool>>) {
        if let Some(p) = self.panes.get_mut(pane) {
            p.stop_signal = signal;
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
/// Pane ORDER is preserved: the app's own manager pane keeps its index
/// position in the grid, so focus hotkeys and dispatch routing stay stable.
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
    compute_layout_with_maximized(pane_count, area, None)
}

/// The layout WITH an expanded pane: pane `maximized` takes the whole
/// area, every other pane's slot collapses to a zero rect (the
/// degenerate path `render_pane` already skips). Index positions are
/// PRESERVED — the caller's per-index structures need no re-keying.
#[must_use]
pub fn compute_layout_with_maximized(
    pane_count: usize,
    area: Rect,
    maximized: Option<usize>,
) -> Vec<Rect> {
    if let Some(i) = maximized
        && i < pane_count
    {
        return (0..pane_count)
            .map(|j| {
                if j == i {
                    area
                } else {
                    Rect::new(area.x, area.y, 0, 0)
                }
            })
            .collect();
    }
    compute_layout_grid(pane_count, area)
}

/// The normal grid: the bin-packing heuristic.
#[must_use]
fn compute_layout_grid(pane_count: usize, area: Rect) -> Vec<Rect> {

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
    let rects = compute_layout_with_maximized(state.pane_count(), area, state.maximized);
    state.pane_rects = rects.clone();
    // Every pane gets a region slot for THIS frame: panes that do not
    // render (degenerate rects) contribute an empty list, so extraction
    // over them finds nothing instead of reading a stale frame's regions.
    state.pane_regions.clear();
    state.pane_regions.resize(state.pane_count(), Vec::new());
    let focused = state.focused;
    // Active drag-selection span in content rows, for the anchor pane only:
    // rows inside it get selection regions even when scrolled out of view,
    // so a release after an auto-scroll drag copies the whole span (the
    // chat rebuilds regions for the span on release; panes emit them during
    // the frame instead — same coverage, no second layout pass).
    let select_sel: Option<(usize, i32, i32)> = state
        .selection_pane
        .filter(|_| state.drag_selection.is_some())
        .map(|p| {
            let a = state.selection_anchor_content_y;
            let f = state.selection_focus_content_y;
            (p, a.min(f), a.max(f))
        });
    // Split the borrows once: the per-pane loop needs `&mut panes[i]`,
    // `&mut block_cache` and `&mut pane_regions[i]` simultaneously (the
    // cache is keyed per pane, the regions per pane slot).
    let AgentsSplitState {
        panes,
        block_cache,
        diff_cells,
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
        let select_span = select_sel
            .filter(|(p, _, _)| *p == i)
            .map(|(_, lo, hi)| (lo, hi));
        render_pane(
            buf,
            rect,
            i,
            pane,
            Some(i) == focused,
            theme,
            block_cache,
            diff_cells,
            &mut live_keys,
            &mut pane_regions[i],
            select_span,
        );
    }
    block_cache.retain(|key, _| live_keys.contains(key));
    // The EditDiff cell cache rides the SAME live-key set (the arm keys
    // itself `{pane_index}:diff{index}` into it), so its pruning is one
    // pass too — the right panel's window prunes identically.
    diff_cells.retain(|key, _| live_keys.contains(key));
    // Hover highlight over a pane's input box (the chat's
    // `render_hover_highlight` recolor, pane scoped): the click-ability
    // cue. Painted after the panes so it lightens the final cells.
    if let Some((pane_index, hit)) = state.hovered_input.as_ref()
        && let (Some(pane), Some(rect)) =
            (state.panes.get(*pane_index), state.pane_rects.get(*pane_index).copied())
    {
        let body_top = i32::from(rect.y + 1);
        let body_end = i32::from(rect.bottom());
        for row in hit.span.clone() {
            let sy = body_top + row - i32::from(pane.effective_scroll);
            if sy < body_top || sy >= body_end {
                continue;
            }
            let y = u16::try_from(sy).unwrap_or(rect.y);
            for x in rect.x..rect.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(rgba_color(theme.background_element));
                }
            }
        }
    }
    // Copy-by-selection highlight, painted LAST so it inverts the final
    // cells (the right panel paints its highlight the same way).
    highlight_selection(buf, state);
}

/// Paint one pane: element background, the agent-name header and
/// the tail of the activity stream (or the idle hint) — rendered through
/// the right panel's REAL pipeline (`activity_lines` + `SubagentBlockRenderer`),
/// so markdown blocks wrap, style and cache exactly like the sub-agent
/// mini-chat instead of a stripped-down mirror.
///
/// `select_span` is the active drag-selection span in content rows (for
/// THIS pane; `None` otherwise): rows inside it report selection regions
/// even when scrolled out of view, so the release copies the whole span.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    buf: &mut Buffer,
    rect: Rect,
    pane_index: usize,
    pane: &mut AgentPane,
    focused: bool,
    theme: &Theme,
    block_cache: &mut HashMap<String, SubagentBlockRenderer>,
    diff_cells: &mut HashMap<String, SubagentDiffCells>,
    live_keys: &mut HashSet<String>,
    out_regions: &mut Vec<TextRegion>,
    select_span: Option<(i32, i32)>,
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
    // The BRAND casing ("OpenCode"), not the lowercase ACP id — the id
    // stays the dispatch key; only the painted label changes. The palette
    // lookup is case-insensitive, so the brand colors still resolve.
    let label = display_name(&pane.agent).to_string();
    let label_w = label.chars().count() as u16;
    // The pane's ACP budget, top-right: the SAME pair the session header
    // paints, fed by the harness's `UsageUpdate` snapshot. The component
    // picks the widest variant that fits the pane's COLUMNS — the full
    // `used / size tok` + bar on a wide pane, the compact `9.6k/100k 9%`
    // on a narrow one. The variant is chosen AFTER reserving the logo
    // (breathing + name + separator + right margin), so the budget
    // shrinks to minimal instead of covering the name while the name can
    // still be kept. No snapshot yet (or not even the compact pair fits
    // beside the logo) → no budget, like the header hiding its price
    // when nothing is known.
    let mut name_end = rect.right();
    if let Some((context_window, tokens_in_context)) = pane.activity.usage {
        let reserved = label_w.saturating_add(3);
        let avail = rect.width.saturating_sub(reserved);
        if let Some(block) = budget_block(tokens_in_context, context_window, avail) {
            let block_w = block.width();
            let block_x = rect.right().saturating_sub(block_w + 1).max(rect.x);
            let token_style = Style::default().fg(rgba_color(theme.text_muted));
            for (i, ch) in block.counter.chars().enumerate() {
                let Ok(i) = u16::try_from(i) else { break };
                let Some(cx) = block_x.checked_add(i) else {
                    break;
                };
                if cx >= rect.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, rect.y)) {
                    cell.set_char(ch);
                    cell.set_style(token_style);
                }
            }
            let bar_style = budget_bar_style(block.pct, theme);
            let pct_style = budget_pct_style(block.pct, theme);
            let bar_x = block_x.saturating_add(block.counter.chars().count() as u16 + block.gap);
            for (i, ch) in block.meter.chars().enumerate() {
                let Ok(i) = u16::try_from(i) else { break };
                let Some(cx) = bar_x.checked_add(i) else {
                    break;
                };
                if cx >= rect.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, rect.y)) {
                    cell.set_char(ch);
                    cell.set_style(bar_style);
                }
            }
            // The percentage reads bold, in the bar's own tone.
            let pct_x = bar_x.saturating_add(block.meter.chars().count() as u16);
            for (i, ch) in block.pct_text.chars().enumerate() {
                let Ok(i) = u16::try_from(i) else { break };
                let Some(cx) = pct_x.checked_add(i) else {
                    break;
                };
                if cx >= rect.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, rect.y)) {
                    cell.set_char(ch);
                    cell.set_style(pct_style);
                }
            }
            // One separator column between the clipped name and the block.
            name_end = block_x.saturating_sub(1);
        }
    }
    let name_w = name_end.saturating_sub(name_x);
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
    // The input-box registry is THIS frame's: stale spans from a previous
    // layout must never light a row the current layout does not own.
    pane.input_rows.clear();
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
    // Input lines (the sent boxes) reserve their BREATHING rows in this
    // pane's flow: one pad row above + one below the markdown block (the
    // chat's border+padding). The pads are real flow rows — the first
    // input's top pad exists (it is not a frame decoration clamped away)
    // and the bottom pad is a margin the following output flows UNDER,
    // never over.
    let content_height: u16 = lines
        .iter()
        .map(|l| {
            l.visual_rows(wrap_w)
                .saturating_add(u16::from(is_input_line(l)) * INPUT_BOX_PAD_ROWS)
        })
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
    // Lines overlapping the active selection span are visited even when
    // fully off-window: they paint nothing but still report regions, so a
    // release after an auto-scroll drag copies the whole span.
    let theme_key = subagent_theme_key(theme);
    // Same whisper-lighter background for markdown blocks so they merge
    // with the pane instead of floating as distinct boxes.
    let tint = RGBA::from_hex("#0A0A0D");
    let mut regions: Vec<TextRegion> = Vec::new();
    let mut content_row: u16 = 0;
    // The running USER-input ordinal: the k-th user echo of the timeline
    // (the positional key into the pane session's user messages).
    let mut user_inputs: usize = 0;
    for (index, line) in lines.iter().enumerate() {
        let h = line.visual_rows(wrap_w);
        let input = is_input_line(line);
        // Input boxes own their breathing rows IN THE FLOW: the line's
        // extent is [content_row, content_row + h + pads) — one pad row
        // above the block, one below. The first input's top pad is a real
        // row (it exists even at content row 0 — nothing clamps it away),
        // and the bottom pad is a margin the following output flows UNDER,
        // never over. Non-input lines keep the bare block extent.
        let top_pad = u16::from(input) * (INPUT_BOX_PAD_ROWS / 2);
        let line_end = content_row
            .saturating_add(h)
            .saturating_add(u16::from(input) * INPUT_BOX_PAD_ROWS);
        // Selection-span overlap in this line's ri-relative rows
        // (end-exclusive), so off-window rows still report regions.
        let emit_rows: Option<(u16, u16)> = select_span.and_then(|(lo, hi)| {
            let elo = lo
                .saturating_sub(i32::from(content_row))
                .clamp(0, i32::from(h)) as u16;
            let ehi = hi
                .saturating_add(1)
                .saturating_sub(i32::from(content_row))
                .clamp(0, i32::from(h)) as u16;
            (elo < ehi).then_some((elo, ehi))
        });
        if line_end <= scroll_y && emit_rows.is_none() {
            // Entirely above the window and outside the selection.
            content_row = line_end;
            continue;
        }
        if content_row >= scroll_y.saturating_add(body_h) && emit_rows.is_none() {
            break;
        }
        // Off-window lines overlapping the span emit regions only: no
        // screen position, no painted rows.
        let visible =
            line_end > scroll_y && content_row < scroll_y.saturating_add(body_h);
        if input {
            // The box's full extent: pad row above, content rows, pad row
            // below — the clickable surface, hover target and drag anchor.
            // The hit carries the box's identity: its timeline entry
            // (Copy) and, for a USER echo, its user ordinal (the
            // Revert/Fork target in the pane session).
            let is_user_echo = matches!(line, SubagentActivityLine::UserMessage { .. });
            let user_ordinal = is_user_echo.then(|| {
                let ordinal = user_inputs;
                user_inputs += 1;
                ordinal
            });
            // The line's OWN `index` is the timeline position (activity_lines
            // stamps Message-block lines from the timeline enumerate) — the
            // walk's `index` is the LINES position, which skips empty
            // entries and would drift.
            let timeline_index = match line {
                SubagentActivityLine::UserMessage { index, .. }
                | SubagentActivityLine::AgentMessage { index, .. } => *index,
                _ => index,
            };
            pane.input_rows.push(InputBoxHit {
                span: i32::from(content_row)..i32::from(line_end),
                timeline_index,
                user_ordinal,
            });
            // The frame paints the WHOLE visible box band in ONE call,
            // BEFORE the content blit: the rails sit on the pane
            // background outside the blit's `[x, x + wrap_w)` span, the
            // band fills the interior, and the markdown bg (the panel
            // tone) overwrites the band inside the blit exactly like the
            // chat's parts overwrite theirs. The band is CLAMPED to the
            // visible window: a box straddling the window top paints only
            // its lower rows and never bleeds into the header or a
            // neighbor pane.
            let box_top = content_row.max(scroll_y);
            let box_bottom = line_end.min(scroll_y.saturating_add(body_h));
            if box_bottom > box_top {
                let frame_y = body_y + box_top.saturating_sub(scroll_y);
                let frame_h = box_bottom - box_top;
                let rail = if matches!(line, SubagentActivityLine::UserMessage { .. }) {
                    brand_rgba(&pane.agent, 0.0, theme, focused)
                } else {
                    theme.secondary
                };
                let frame = Rect::new(
                    rect.x,
                    frame_y,
                    wrap_w.saturating_add(2),
                    frame_h,
                );
                prompt_message_box::render(buf, frame, rail, theme);
            }
        }
        // The BLOCK (the markdown content) sits `top_pad` rows below the
        // line's start; the screen position and the skip account for it.
        let block_content_row = content_row.saturating_add(top_pad);
        let block_screen_y = body_y + block_content_row.saturating_sub(scroll_y);
        let block_skip = scroll_y.saturating_sub(block_content_row);
        let draw_rows = if visible {
            h.saturating_sub(block_skip)
                .min(rect.bottom().saturating_sub(block_screen_y))
        } else {
            0
        };
        if draw_rows > 0 || emit_rows.is_some() {
            draw_activity_line(
                buf,
                line,
                index,
                pane_index,
                &pane.agent,
                rect.x + 1,
                block_screen_y,
                wrap_w,
                block_skip,
                draw_rows as usize,
                focused,
                theme,
                theme_key,
                tint,
                block_cache,
                diff_cells,
                live_keys,
                &mut regions,
                block_content_row,
                emit_rows,
            );
        }
        content_row = line_end;
    }
    *out_regions = regions;
}

/// One clickable INPUT box of a pane, as registered by the last render:
/// the screen-independent identity the click handler needs.
#[derive(Debug, Clone, PartialEq)]
pub struct InputBoxHit {
    /// Content-row span of the whole box (pads included) — the hover and
    /// click hit-test target.
    pub span: std::ops::Range<i32>,
    /// The box's timeline entry index (`SubagentActivity.timeline`) —
    /// Copy reads the text straight from it, no store round-trip.
    pub timeline_index: usize,
    /// The k-th USER input of the timeline (`None` for an `agents_call`
    /// input): the positional key into the pane session's user messages —
    /// the same order the echo and the replay builders follow — that
    /// maps the click onto the store's Revert/Fork target.
    pub user_ordinal: Option<usize>,
}

/// Whether this activity line renders as a sent-prompt INPUT box (the
/// user's echo, an `agents_call` input): the clickable, hoverable,
/// frame-carrying surface. The chat's clickable-user-message equivalent.
fn is_input_line(line: &SubagentActivityLine) -> bool {
    matches!(
        line,
        SubagentActivityLine::UserMessage { .. } | SubagentActivityLine::AgentMessage { .. }
    )
}

/// Union of a line's painted band `[skip, skip + paint)` with the
/// selection-span band `emit` (both ri-relative, end-exclusive), clamped to
/// the line's `len` rows: the single row range an activity-line arm below
/// iterates — it paints only the painted band but reports selection regions
/// for the whole union, so one loop serves both without duplicating the
/// per-row text formatting.
fn paint_emit_window(skip: u16, paint: usize, emit: Option<(u16, u16)>, len: usize) -> (u16, u16) {
    let (mut lo, mut hi) = (skip, skip.saturating_add(paint.min(u16::MAX as usize) as u16));
    if let Some((elo, ehi)) = emit {
        lo = lo.min(elo);
        hi = hi.max(ehi);
    }
    let len = len.min(u16::MAX as usize) as u16;
    (lo.min(len), hi.min(len))
}

/// Paint ONE drawable activity line at absolute `(x, y)`, clipped to
/// `draw_rows` with its first `skip` rows scrolled off. `emit_rows` is an
/// optional ri-relative row band (end-exclusive) that reports selection
/// regions WITHOUT painting — the active drag-selection span where it
/// overlaps this line — so off-window rows still copy. Markdown blocks
/// (`Message`/`Thought`) go through the pane's persistent
/// `SubagentBlockRenderer` (chat-style incremental parsing, scratch-buffer
/// blit — the exact pattern of the right panel's live window); Tool/Diff/
/// Plan lines draw with the same marker/glyph styles. The replayed user
/// block additionally wears the prompt input's left `│` rail (see the
/// `UserMessage` arm). Every painted visual
/// row is reported as a `TextRegion` labeled with its CONTENT row, so
/// copy-by-selection reads exactly what is on screen.
#[allow(clippy::too_many_arguments)]
fn draw_activity_line(
    buf: &mut Buffer,
    line: &SubagentActivityLine,
    index: usize,
    pane_index: usize,
    _agent: &str,
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
    diff_cells: &mut HashMap<String, SubagentDiffCells>,
    live_keys: &mut HashSet<String>,
    regions: &mut Vec<TextRegion>,
    content_row: u16,
    emit_rows: Option<(u16, u16)>,
) {
    // Painted band in ri-relative rows: the union loop below paints only
    // these, but reports regions for `emit_rows` too.
    let paint_hi = skip.saturating_add(draw_rows.min(u16::MAX as usize) as u16);
    let painting = |ri: u16| ri >= skip && ri < paint_hi;
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
            let (rlo, rhi) = paint_emit_window(skip, draw_rows, emit_rows, rows.len());
            for ri in rlo..rhi {
                let ri = ri as usize;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + (ri as u16).saturating_sub(skip);
                if painting(ri as u16) {
                    if ri == 0 {
                        draw_text(buf, marker, x, line_y, wrap_w, marker_style);
                    }
                    draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), text_style);
                }
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
        SubagentActivityLine::EditDiff {
            index,
            path,
            unified,
        } => {
            // The REAL unified diff of a completed file edit — the SAME
            // `DiffRenderable` panel the chat's edit box paints, blitted
            // from the per-line cell cache (parse + tree-sitter highlight
            // once per change, not once per frame). The panel sits
            // indented at x+2, under the compact `path +N −M` summary
            // line above it. Every visible row reports a content-only
            // region sampled from the same cells, so copy-by-selection
            // reads the plain `+`/`-` lines.
            let panel_w = wrap_w.saturating_sub(2);
            let key = format!("{pane_index}:diff{index}");
            live_keys.insert(key.clone());
            let rows = usize::from(subagent_diff_rows(unified, panel_w));
            let content_key = subagent_diff_content_key(path, unified);
            let stale = diff_cells.get(&key).is_none_or(|e| {
                e.wrap_w != wrap_w || e.theme_key != theme_key || e.content_key != content_key
            });
            if stale {
                let cells = subagent_diff_block(unified, path, panel_w, theme);
                diff_cells.insert(
                    key.clone(),
                    SubagentDiffCells {
                        wrap_w,
                        theme_key,
                        content_key,
                        cells,
                    },
                );
            }
            let Some(entry) = diff_cells.get(&key) else {
                return;
            };
            let (rlo, rhi) = paint_emit_window(skip, draw_rows, emit_rows, rows);
            for ri in rlo..rhi {
                let ri = ri as usize;
                let line_y = y + (ri as u16).saturating_sub(skip);
                let base = ri * usize::from(panel_w);
                // Defensive: the cache must hold `rows` full panels, but an
                // emit-only row must never index past a short cache.
                if base.saturating_add(usize::from(panel_w)) > entry.cells.len() {
                    break;
                }
                if painting(ri as u16) {
                    for dx in 0..panel_w {
                        let Some(dst) = buf.cell_mut((x + 2 + dx, line_y)) else {
                            break;
                        };
                        *dst = entry.cells[base + usize::from(dx)].clone();
                    }
                }
                let row_cells: Vec<ratatui::buffer::Cell> = (0..panel_w)
                    .map(|dx| entry.cells[base + usize::from(dx)].clone())
                    .collect();
                let trimmed = text_from_cell_row(&row_cells, panel_w as usize);
                if !trimmed.is_empty() {
                    regions.push(TextRegion::one_row(
                        i32::from(content_row.saturating_add(ri as u16)),
                        x,
                        x + wrap_w,
                        trimmed,
                    ));
                }
            }
        }
        SubagentActivityLine::Diff(rows) => {
            let (rlo, rhi) = paint_emit_window(skip, draw_rows, emit_rows, rows.len());
            for ri in rlo..rhi {
                let ri = ri as usize;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + (ri as u16).saturating_sub(skip);
                if painting(ri as u16) {
                    draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), dimmed_style);
                }
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
            let (rlo, rhi) = paint_emit_window(skip, draw_rows, emit_rows, rows.len());
            for ri in rlo..rhi {
                let ri = ri as usize;
                let Some(seg) = rows.get(ri) else { break };
                let line_y = y + (ri as u16).saturating_sub(skip);
                if painting(ri as u16) {
                    if ri == 0 {
                        draw_text(buf, glyph, x, line_y, wrap_w, style);
                    }
                    draw_text(buf, seg, x + 2, line_y, wrap_w.saturating_sub(2), style);
                }
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
        | SubagentActivityLine::AgentMessage { text, .. }
        | SubagentActivityLine::Thought { text, .. } => {
            // Persistent chat-style renderer per (pane, timeline) block —
            // `set_content` re-parses only the streamed tail. Thought is
            // dimmed, exactly like the right panel's mini-chat; the
            // replayed user block renders at full text color and wears the
            // prompt input's left `│` rail (painted after each blitted
            // row, below); an `agents_call` input wears the SAME rail in
            // the theme's secondary tone — the user sees WHO is speaking
            // from the rail's color alone.
            let dimmed = matches!(line, SubagentActivityLine::Thought { .. });
            let is_user = matches!(line, SubagentActivityLine::UserMessage { .. });
            let is_agent_input = matches!(line, SubagentActivityLine::AgentMessage { .. });
            // The chat's sent box BREATHES: one padding row above and one
            // below the content (its height math adds border 1 + padding 1
            // to the user message). The pane's input box mirrors that: the
            // frame paints `PROMPT_BOX_PAD` rows above and below the
            // markdown block (see the frame guards below) — padding is
            // chrome, it never enters the content rows' regions or the
            // copied text.
            let fg = if dimmed { theme.text_muted } else { theme.text };
            // The input blocks (the user's echo, an `agents_call` input)
            // render on the chat's sent-box PANEL band — the frame the
            // `prompt_message_box` component paints under them carries
            // `background_panel`, so the markdown bg must MATCH it or the
            // blit would overwrite the band and the box would read as bare
            // text. Every other block keeps the pane's whisper tint.
            let is_input = is_user || is_agent_input;
            let block_bg = if is_input { theme.background_panel } else { tint };
            let key = format!("{pane_index}:{index}");
            // This block is drawn THIS frame: its cache entry is live (the
            // split-level `retain` drops every key not reported here).
            live_keys.insert(key.clone());
            let stale = block_cache.get(&key).is_none_or(|e| {
                e.wrap_w != wrap_w
                    || e.theme_key != theme_key
                    || e.bg != block_bg
                    || e.dimmed != dimmed
            });
            if stale {
                let mut md = MarkdownRenderable::new(Some(text.clone()));
                md.set_fg(Some(ColorInput::RGBA(fg)));
                md.set_bg(Some(ColorInput::RGBA(block_bg)));
                crate::util::markdown::apply_theme(&mut md, theme);
                let area = Rect::new(0, 0, wrap_w, h_from(text, wrap_w));
                block_cache.insert(
                    key.clone(),
                    SubagentBlockRenderer {
                        wrap_w,
                        theme_key,
                        bg: block_bg,
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
            // oversized block naturally shows its tail). Every row in the
            // union with the selection span becomes a content-row region
            // sampled from the same cells, so extraction matches the
            // display — including rows scrolled out of view.
            let block_h = h_from(text, wrap_w) as usize;
            let (rlo, rhi) = paint_emit_window(skip, draw_rows, emit_rows, block_h);
            for sy in rlo..rhi {
                let paint_row = painting(sy);
                let dy = sy.saturating_sub(skip);
                if paint_row && is_input {
                    // The chat's sent-prompt FRAME (the extracted
                    // `component::prompt_message_box`): `│` rails on BOTH
                    // sides + the panel band, the same geometry the main
                    // chat's submitted message paints. The band goes down
                    // BEFORE the blit — the content overwrites it exactly
                    // like the chat's parts overwrite the band — and the
                    // rails sit one column OUTSIDE the blit's
                    // `[x, x + wrap_w)` span, so they survive it. Painted
                    // per VISIBLE row: a block straddling the pane's top
                    // edge must never bleed its frame into the header row
                    // or a neighbor pane.
                    //
                    // The FRAME (and its speaker TONE) is painted by the
                    // walk — one call over the whole visible box band,
                    // BEFORE this blit. This loop only blits the content
                    // rows; the rails survive the blit because they sit
                    // outside `[x, x + wrap_w)`.
                }
                if paint_row {
                    for cx in 0..wrap_w {
                        let Some(src) = entry.scratch.cell((cx, sy)) else {
                            break;
                        };
                        let Some(dst) = buf.cell_mut((x + cx, y + dy)) else {
                            break;
                        };
                        dst.set_symbol(src.symbol());
                        dst.set_style(src.style());
                    }
                }
                let row_cells: Vec<ratatui::buffer::Cell> = (0..wrap_w)
                    .map(|dx| entry.scratch.cell((dx, sy)).cloned().unwrap_or_default())
                    .collect();
                let trimmed = text_from_cell_row(&row_cells, wrap_w as usize);
                if !trimmed.is_empty() {
                    // Content-only, like the chat's regions: the input's
                    // frame (rails + band) is PAINT-only (drawn ABOVE the
                    // blit, OUTSIDE this `[x, x + wrap_w)` span) and never
                    // enters the copied text — selecting an input copies
                    // exactly what was typed, no decoration prefix.
                    regions.push(TextRegion::one_row(
                        i32::from(content_row.saturating_add(sy)),
                        x,
                        x + wrap_w,
                        trimmed,
                    ));
                }
            }
        }
        SubagentActivityLine::Gap => {
            // The breathing row between the user's input and the agent's
            // output: the pane's background fill already painted it —
            // nothing to draw, no selection region (an empty row is not
            // content).
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
    for (i, (ch, style)) in chars
        .into_iter()
        .zip(brand_name_styles(agent, theme, focused))
        .enumerate()
    {
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

/// The style of ONE glyph column of an agent's brand name, shared by the
/// split-pane headers and the right panel's subagent list (the bound
/// `/agents` agents get the same full-color logo there; normal subagents
/// pass `None`'s palette branch and stay in the theme's own colors —
/// opaque, never a half-brand tint).
///
/// A brand mark never dissolves into the surface: when the sampled color's
/// luminance sits too close to the background's, it lifts toward the
/// readable contrast color instead.
/// The brand color as a raw RGBA (the rail/frame painters want the color
/// itself, not a `Style`): the exact tone [`brand_fg`] wraps.
pub(crate) fn brand_rgba(agent: &str, t: f32, theme: &Theme, focused: bool) -> RGBA {
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
            if focused {
                brand
            } else {
                blend(theme.background_element, brand, DIM_UNFOCUSED)
            }
        }
        None => {
            if focused {
                theme.text
            } else {
                theme.text_muted
            }
        }
    }
}

pub(crate) fn brand_fg(agent: &str, t: f32, theme: &Theme, focused: bool) -> Style {
    Style::default().fg(rgba_color(brand_rgba(agent, t, theme, focused)))
}

/// The `│` INPUT rail marking a turn's INPUT (the user's echo in an
/// `/agents` pane, the main agent's echoed prompt in a right-panel
/// subagent window) — ONE glyph, paint-only, drawn in the column LEFT of
/// the content: the same marking on both surfaces, so "who is speaking"
/// reads identically in the split and in the panel. The STYLE is the
/// caller's tone decision (the pane paints the user's rail in the agent's
/// brand color and another agent's input in the theme's secondary tone;
/// the right panel's echoed prompt is always the main agent's —
/// secondary) — the GLYPH, the position and the paint-only contract live
/// in this one function. Input text renders WITHOUT the rail in its
/// selection regions — copying an input copies exactly what was typed,
/// no decoration prefix.
///
/// Shared by BOTH render paths (this module's pane renderer and the
/// right panel's window renderer) — a future restyle changes this one
/// function, never a per-surface copy.
pub(crate) fn subagent_input_rail(buf: &mut Buffer, x: u16, y: u16, style: Style) {
    if let Some(rail) = buf.cell_mut((x, y)) {
        rail.set_char('│');
        rail.set_style(style);
    }
}

/// One BOLD brand style per glyph of an agent's display name — the exact
/// sweep [`draw_agent_name`] paints (gradient marks walk their colors
/// left to right, single-color marks stay solid, unknown agents keep the
/// theme's text). Shared by the pane header and the `/agents` prompt
/// badge so the two logos can never drift apart; the caller paints the
/// background (the header relies on its fill, the badge on the row's).
pub(crate) fn brand_name_styles(agent: &str, theme: &Theme, focused: bool) -> Vec<Style> {
    let count = agent.chars().filter(|c| !c.is_control()).count();
    agent
        .chars()
        .filter(|c| !c.is_control())
        .enumerate()
        .map(|(i, _)| {
            // One sample per glyph column: `t` walks the mark's colors
            // left to right (constant for single-color marks).
            let t = if count <= 1 {
                0.0
            } else {
                i as f32 / (count as f32 - 1.0)
            };
            brand_fg(agent, t, theme, focused).add_modifier(Modifier::BOLD)
        })
        .collect()
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
/// never paints into a neighbour). Both endpoints live in content space
/// and are projected with the CURRENT render scroll, so the visual band
/// follows the content when it scrolls mid-drag — the chat's contract.
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
    // Both endpoints track the CURRENT render scroll (the chat projects
    // both with its live `scroll_y` too): the band follows the content
    // when it scrolls mid-drag instead of pinning the anchor edge to the
    // press row.
    let anchor_screen = anchor_content
        .saturating_sub(i32::from(pane.effective_scroll))
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

        // An explicitly marked cosh keeps its marked position (first) and is
        // NOT duplicated.
        let state = AgentsSplitState::new(vec![DAEMON_LABEL.into(), "claude".into()]);
        assert_eq!(state.pane_count(), 2);
        assert_eq!(state.panes[0].agent, "cosh");
        assert_eq!(state.panes[1].agent, "claude");
        // The cosh pane is addressable like every other agent: its registry
        // id is `"cosh"`, not `None`.
        assert_eq!(state.pane_registry_id(0).as_deref(), Some("cosh"));
        assert_eq!(state.pane_registry_id(1).as_deref(), Some("claude"));
    }

    #[test]
    fn cosh_panes_are_registry_addressable_with_occurrence_suffixes() {
        let state =
            AgentsSplitState::new(vec![DAEMON_LABEL.into(), "claude".into(), DAEMON_LABEL.into()]);
        assert_eq!(state.pane_registry_id(0).as_deref(), Some("cosh#1"));
        assert_eq!(state.pane_registry_id(1).as_deref(), Some("claude"));
        assert_eq!(state.pane_registry_id(2).as_deref(), Some("cosh#2"));
        assert!(state.pane_registry_id(3).is_none(), "out-of-range pane");
    }

    #[test]
    fn focus_hotkey_targets_panes_by_position() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        // '1' targets the first pane (claude), '2' the second (kilo) —
        // hotkeys count panes 1-based; '0' is the main agent, never a pane.
        assert_eq!(state.focus_hotkey('2'), Some(1));
        assert_eq!(state.focused, Some(1));
        assert_eq!(state.focused_pane().map(|p| p.agent.as_str()), Some("kilo"));
        assert_eq!(state.focus_hotkey('1'), Some(0));
        assert_eq!(state.focused, Some(0));
        assert_eq!(state.focused_pane().map(|p| p.agent.as_str()), Some("claude"));
        // '0' UNFOCUSES every pane: the shared prompt then targets the
        // MAIN agent (the normal-mode loop as manager).
        assert_eq!(state.focus_hotkey('0'), None);
        assert_eq!(state.focused, None);
        assert!(state.focused_pane().is_none(), "no pane holds the focus");
        // A pane hotkey focuses again from the unfocused state.
        assert_eq!(state.focus_hotkey('2'), Some(1));
        assert_eq!(state.focused, Some(1));
        // Out-of-range and unknown keys leave the focus alone.
        assert_eq!(state.focus_hotkey('3'), None);
        assert_eq!(state.focused, Some(1));
        assert_eq!(state.focus_hotkey('9'), None);
        assert_eq!(state.focused, Some(1));
        assert_eq!(state.focus_hotkey('x'), None);
        assert_eq!(state.focused, Some(1));
    }

    #[test]
    fn refocus_returns_to_the_first_pane() {
        let mut state = AgentsSplitState::new(vec!["claude".into(), "kilo".into()]);
        // Fresh splits start on the first pane; unfocusing and focusing
        // again lands back on it.
        assert_eq!(state.focused, Some(0));
        assert_eq!(state.focus_hotkey('0'), None);
        assert_eq!(state.focused, None);
        assert_eq!(state.focus_hotkey('1'), Some(0));
        assert_eq!(state.focused, Some(0));
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

    /// Regression (the panes' garbled order): two tool-less turns through
    /// the full pane path — echo, stream, `finish_turn`, echo, stream,
    /// `finish_turn` — must read as a linear chronological flow like the
    /// normal chat. The second answer used to skip over its own prompt and
    /// merge into the first turn's report block, so the pane showed one
    /// atropelada paragraph instead of prompt/answer/prompt/answer.
    #[test]
    fn consecutive_pane_turns_keep_their_chronological_entries() {
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        // Turn 1.
        state.echo_turn_input(0, "ola como vai?", true);
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "tudo bem! ".to_string(),
            },
        );
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "como posso ajudar?".to_string(),
            },
        );
        state.finish_turn(0, "tudo bem! como posso ajudar?", "end_turn");
        // Turn 2 (no tool call in between — the old merge crossed here).
        state.echo_turn_input(0, "qual seu nome?", true);
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "sou o Kilo.".to_string(),
            },
        );
        state.finish_turn(0, "sou o Kilo.", "end_turn");

        let rows: Vec<(bool, String)> = state.panes[0]
            .activity
            .timeline
            .iter()
            .map(|entry| match entry {
                SubagentTimelineEntry::UserMessage { text } => (true, text.clone()),
                SubagentTimelineEntry::Message { text } => (false, text.clone()),
                other => panic!("expected prompt/answer rows, got {other:?}"),
            })
            .collect();
        assert_eq!(
            rows.iter().map(|(user, _)| *user).collect::<Vec<_>>(),
            vec![true, false, true, false],
            "prompt, answer, prompt, answer — in arrival order: {rows:?}"
        );
        assert!(
            rows[1].1.contains("como posso ajudar?"),
            "the first answer stays its own row: {rows:?}"
        );
        assert!(
            rows[3].1.contains("sou o Kilo."),
            "the second answer stays its own row: {rows:?}"
        );
        assert!(
            !rows[1].1.contains("Kilo"),
            "the second answer never leaks into the first: {rows:?}"
        );
    }

    /// The input's `│` rail is paint-only: drag-selecting an input copies
    /// exactly what was typed, never the decoration — the chat's
    /// content-only region contract (its rails sit outside the regions).
    /// The sent-prompt FRAME on a pane input: the extracted chat box —
    /// `│` rails on BOTH sides of the input block plus the panel band —
    /// with the USER's echo wearing the pane agent's brand tone and an
    /// `agents_call` input wearing the theme's secondary. The shape is the
    /// component's contract; the rail TONE is the speaker.
    #[test]
    fn pane_input_paints_the_sent_prompt_frame_with_speaker_tones() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        // The user's echo (brand tone)…
        state.echo_turn_input(0, "ola como vai?", true);
        // …and an agent-sent input (secondary tone) in the SAME pane.
        state.echo_turn_input(0, "and this too", false);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let body_top = area.y + 1;
        // Both rails of the input frame: `│` glyphs on the frame columns
        // (one OUTSIDE the content span on each side), full input height.
        let mut rows_with_rails = 0;
        let mut band_seen = false;
        let panel = theme.background_panel;
        let (pr, pg, pb, _) = panel.to_ints();
        let band = ratatui::style::Color::Rgb(pr, pg, pb);
        for dy in 0..area.height - 1 {
            let y = body_top + dy;
            let mut rails = 0u32;
            for cx in 0..area.width {
                if let Some(c) = buf.cell((cx, y)) {
                    if c.symbol() == "\u{2502}" {
                        rails += 1;
                    }
                    if c.bg == band {
                        band_seen = true;
                    }
                }
            }
            if rails >= 2 {
                rows_with_rails += 1;
            }
        }
        assert!(
            rows_with_rails >= 2,
            "the frame paints both rails on the input rows: {rows_with_rails}"
        );
        assert!(band_seen, "the panel band paints under the input");
    }

    /// The frame TONE follows the speaker (kept dynamics): the USER's
    /// echo wears the pane agent's BRAND color, an `agents_call` input
    /// wears the theme's SECONDARY — the color alone tells WHO is speaking.
    #[test]
    /// The sent box BREATHES like the chat's: a pad row above and below
    /// the content, painted by the frame (the chat's border+padding rows).
    #[test]
    fn pane_input_box_carries_the_chat_breathing_rows() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        // An answer FIRST, so the input box sits past content row 0 and its
        // top pad row has a real content coordinate (the first box's top
        // pad clamps at 0 — there is no row above the timeline start).
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "answer".to_string(),
            },
        );
        state.echo_turn_input(0, "breathing", true);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let rect = state.pane_rects[0];
        // The content rows of the input box (registry: pad-inclusive), so
        // the pad rows are the span's FIRST and LAST rows. The box sits
        // past content row 0 (an answer precedes it), so its full
        // pad+content+pad extent exists.
        let pane = &state.panes[0];
        let hit = pane.input_rows.last().expect("input box registered");
        assert_eq!(
            hit.span.len() as u16,
            3,
            "1 pad + 1 content + 1 pad: {:?}",
            hit.span
        );
        assert_eq!(hit.user_ordinal, Some(0), "a user echo carries its ordinal");
        let pad_top = i32::from(rect.y + 1) + hit.span.start - i32::from(pane.effective_scroll);
        let pad_bottom =
            i32::from(rect.y + 1) + hit.span.end - 1 - i32::from(pane.effective_scroll);
        let body_top = i32::from(rect.y + 1);
        let panel = theme.background_panel;
        let (pr, pg, pb, _) = panel.to_ints();
        let band = ratatui::style::Color::Rgb(pr, pg, pb);
        for (label, sy) in [("top pad", pad_top), ("bottom pad", pad_bottom)] {
            assert!(sy >= body_top, "{label} must not bleed into the header");
            let y = u16::try_from(sy).unwrap();
            for cx in rect.x + 1..rect.right() - 1 {
                let cell = buf.cell((cx, y)).unwrap();
                assert_eq!(cell.bg, band, "{label} row carries the band (x={cx})");
                assert_ne!(cell.symbol(), "b", "{label} row carries no content");
            }
        }
    }

    /// The hover highlight (the click cue): a pane input box under the
    /// cursor lights with `background_element`, the chat's
    /// hovered-user-message recolor, pane scoped.
    #[test]
    fn pane_input_hover_lights_the_box() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.echo_turn_input(0, "hover me", true);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let rect = state.pane_rects[0];
        let pane = &state.panes[0];
        let hit = pane.input_rows.first().expect("input box registered");
        // The hover target: a row inside the box (content row -> screen).
        let hover_row = hit.span.start + 1;
        let hover_y = (i32::from(rect.y + 1) + hover_row - i32::from(pane.effective_scroll)) as u16;
        let hover_x = rect.x + 4;

        // Before: some band tone (NOT the hover tone).
        let element = theme.background_element;
        let (er, eg, eb, _) = element.to_ints();
        let hover_bg = ratatui::style::Color::Rgb(er, eg, eb);
        assert_ne!(
            buf.cell((hover_x, hover_y)).unwrap().bg,
            hover_bg,
            "unhovered box does not carry the hover tone"
        );

        // Move the cursor onto the box, re-render, check the recolor.
        assert!(state.update_hover(hover_x, hover_y), "hover registers");
        let mut buf2 = Buffer::empty(area);
        render(&mut buf2, area, &mut state, &theme);
        assert_eq!(
            buf2.cell((hover_x, hover_y)).unwrap().bg,
            hover_bg,
            "the hovered box lights with background_element"
        );
        // Off the box clears (the chat's clear_hover contract).
        assert!(state.clear_hover());
        let mut buf3 = Buffer::empty(area);
        render(&mut buf3, area, &mut state, &theme);
        assert_ne!(
            buf3.cell((hover_x, hover_y)).unwrap().bg,
            hover_bg,
            "cleared hover drops the light"
        );
    }

    fn pane_input_rail_tone_follows_the_speaker() {
        let theme = test_theme();
        // `new` focuses pane 0: the expectation uses the FOCUSED brand.
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.echo_turn_input(0, "user speaks", true);
        state.echo_turn_input(0, "agent speaks", false);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        let expected_user = rgba_color(brand_rgba("kilo", 0.0, &theme, true));
        let expected_agent = rgba_color(theme.secondary);
        let body_top = area.y + 1;
        let mut user_rail = None;
        let mut agent_rail = None;
        // Body rows only: the buffer ends at `area.height`.
        for dy in 0..area.height - 1 {
            let y = body_top + dy;
            for cx in 0..area.width {
                let Some(cell) = buf.cell((cx, y)) else {
                    continue;
                };
                if cell.symbol() != "\u{2502}" {
                    continue;
                }
                if cell.fg == expected_user {
                    user_rail = Some(cell.fg);
                } else if cell.fg == expected_agent {
                    agent_rail = Some(cell.fg);
                }
            }
        }
        assert_eq!(user_rail, Some(expected_user), "the user echo wears brand");
        assert_eq!(
            agent_rail, Some(expected_agent),
            "the agents_call input wears secondary"
        );
    }

    /// The input's `│` rail is paint-only: drag-selecting an input copies
    /// exactly what was typed, never the decoration — the chat's
    /// content-only region contract (its rails sit outside the regions).
    #[test]
    fn selecting_a_pane_input_copies_no_rail() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.echo_turn_input(0, "ola como vai?", true);
        state.apply_event(
            0,
            &SubagentEvent::Message {
                text: "tudo bem!".to_string(),
            },
        );
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);

        // Sanity: the rail IS still painted (only the copy changes).
        let body_top = area.y + 1;
        let rail_painted = (0..area.height).any(|dy| {
            buf.cell((area.x, body_top + dy))
                .map(|cell| cell.symbol() == "│")
                .unwrap_or(false)
        });
        assert!(rail_painted, "the rail still paints on screen");

        // Drag across the whole body and copy.
        let bottom = area.y + area.height - 1;
        state.begin_selection(0, area.x, body_top, 0);
        state.update_selection(area.x + area.width - 1, bottom, 20);
        let copied = state.extract_selected_text();
        assert!(
            copied.contains("ola como vai?"),
            "the typed text copies: {copied:?}"
        );
        assert!(
            copied.contains("tudo bem!"),
            "the answer copies too: {copied:?}"
        );
        assert!(
            !copied.contains('│'),
            "no rail glyph in the copy: {copied:?}"
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

    /// Seed one pane with `n` single-row blocks: Message and Thought
    /// alternate because same-kind stream chunks coalesce into one entry.
    fn seed_tall_pane(state: &mut AgentsSplitState, n: usize) {
        for i in 0..n {
            let text = format!("sel row {i:02}");
            if i % 2 == 0 {
                state.apply_event(0, &SubagentEvent::Message { text });
            } else {
                state.apply_event(0, &SubagentEvent::Thought { text });
            }
        }
    }

    /// Selection regions cover the whole drag span even when it scrolled
    /// out of view: rows inside the span report regions during the frame
    /// (the chat rebuilds regions for the span on release instead — same
    /// coverage), so the release copies off-screen rows too.
    #[test]
    fn selection_span_reports_regions_beyond_the_viewport() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        seed_tall_pane(&mut state, 14);
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let mut ys: Vec<i32> = state.pane_regions[0].iter().map(|r| r.y1).collect();
        ys.sort();
        ys.dedup();
        assert!(ys.len() >= 8, "need a scrollable pane, got {ys:?}");
        let anchor = ys[0];
        let focus = ys[7];
        let rect = state.pane_rects[0];

        // Scroll to the bottom: the anchor leaves the viewport.
        {
            let pane = &mut state.panes[0];
            pane.auto_follow = false;
            pane.scroll_y = pane.content_height.saturating_sub(pane.body_height);
        }
        state.selection_pane = Some(0);
        state.selection_anchor_content_y = anchor;
        state.selection_focus_content_y = focus;
        state.drag_selection = Some((rect.x + 1, 2, rect.x + 30, 8));
        let mut buf2 = Buffer::empty(area);
        render(&mut buf2, area, &mut state, &theme);

        let covered: Vec<i32> = state.pane_regions[0].iter().map(|r| r.y1).collect();
        for row in anchor..=focus {
            assert!(
                covered.contains(&row),
                "off-screen row {row} must report a region: {covered:?}"
            );
        }
        let anchor_text = state.pane_regions[0]
            .iter()
            .find(|r| r.y1 == anchor)
            .map(|r| r.text.clone())
            .unwrap_or_default();
        let copied = state.extract_selected_text();
        assert!(
            !anchor_text.is_empty() && copied.contains(&anchor_text),
            "the off-screen anchor row copies: {copied:?}"
        );
    }

    /// The highlight band follows the content when it scrolls mid-drag:
    /// both endpoints project with the CURRENT render scroll (the chat's
    /// contract), instead of pinning the anchor edge to the press row.
    #[test]
    fn highlight_band_tracks_content_when_scrolled_mid_drag() {
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        seed_tall_pane(&mut state, 14);
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        // Pin the viewport to the top and re-render: the rows below come
        // from scroll 0 (fresh panes sticky-follow to the bottom).
        state.panes[0].auto_follow = false;
        state.panes[0].scroll_y = 0;
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let mut ys: Vec<i32> = state.pane_regions[0].iter().map(|r| r.y1).collect();
        ys.sort();
        ys.dedup();
        assert!(ys.len() >= 6, "need a scrollable pane, got {ys:?}");
        let anchor = ys[2];
        let focus = ys[3];
        let rect = state.pane_rects[0];
        let body_top = rect.y + 1;
        let sx = rect.x + 2;
        let fx = rect.x + 20;

        fn changed_rows(sel: &Buffer, base: &Buffer, area: Rect) -> Vec<u16> {
            (area.y..area.bottom())
                .filter(|y| {
                    (area.x..area.right())
                        .any(|x| sel.cell((x, *y)) != base.cell((x, *y)))
                })
                .collect()
        }
        // Render with the selection, then without (same scroll): the only
        // difference between the frames is the highlight band.
        fn band_at(
            state: &mut AgentsSplitState,
            area: Rect,
            theme: &Theme,
            anchor: i32,
            focus: i32,
            sx: u16,
            fx: u16,
        ) -> Vec<u16> {
            state.selection_pane = Some(0);
            state.selection_anchor_content_y = anchor;
            state.selection_focus_content_y = focus;
            state.drag_selection = Some((sx, 0, fx, 0));
            let mut sel = Buffer::empty(area);
            render(&mut sel, area, state, theme);
            state.selection_pane = None;
            state.drag_selection = None;
            let mut base = Buffer::empty(area);
            render(&mut base, area, state, theme);
            changed_rows(&sel, &base, area)
        }

        // At rest the band sits on the anchor/focus rows.
        let screen = |content: i32, scroll: u16| body_top + (content as u16).saturating_sub(scroll);
        assert_eq!(
            band_at(&mut state, area, &theme, anchor, focus, sx, fx),
            vec![screen(anchor, 0), screen(focus, 0)],
            "resting band covers the anchor..focus rows"
        );

        // Scrolled one row down, the whole band moves with the content.
        state.panes[0].scroll_y = 1;
        assert_eq!(
            band_at(&mut state, area, &theme, anchor, focus, sx, fx),
            vec![screen(anchor, 1), screen(focus, 1)],
            "the anchor edge must track the scroll, not the press row"
        );
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
        assert_eq!(state.focus_hotkey('2'), Some(1));
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
        state.focus_hotkey('1');
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

    /// The pane header's right corner carries the ACP budget: the SAME
    /// pair the session header paints (`used / size tok` + bar + pct),
    /// right-aligned with one breathing column at the pane edge, the name
    /// clipping before it. A hot budget takes the header's warning/error
    /// colors; no snapshot yet paints no budget at all.
    #[test]
    fn header_right_corner_paints_the_acp_budget() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 60, 10);
        let row_text = |buf: &Buffer| {
            (0..60)
                .map(|x| {
                    buf.cell((x, 0))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        };

        // 9% used: counter + empty bar, both in the muted tone.
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.panes[0].activity.usage = Some((100_000, 9_612));
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row = row_text(&buf);
        let cols: Vec<char> = row.chars().collect();
        let block = "9,612 / 100,000 tok  \u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  9%";
        let block_cols: Vec<char> = block.chars().collect();
        assert_eq!(block_cols.len(), 35, "block width");
        assert_eq!(
            &cols[24..59],
            &block_cols[..],
            "right-aligned pair: {row:?}"
        );
        assert_eq!(cols[59], ' ', "one breathing column at the edge");
        assert_eq!(
            cols[1..5].iter().collect::<String>(),
            "kilo",
            "the name survives before the block"
        );
        assert_eq!(
            buf.cell((24, 0)).unwrap().style().fg,
            Some(rgba_color(theme.text_muted)),
            "counter in the muted tone"
        );
        assert_eq!(
            buf.cell((45, 0)).unwrap().style().fg,
            Some(rgba_color(theme.text_muted)),
            "cool bar in the muted tone"
        );
        assert!(
            !buf.cell((45, 0))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the bar itself stays plain"
        );
        assert!(
            buf.cell((56, 0))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the percentage reads bold"
        );

        // 95% used: the bar takes the header's hot color.
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.panes[0].activity.usage = Some((100_000, 95_000));
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row = row_text(&buf);
        assert!(
            row.ends_with("95,000 / 100,000 tok  \u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2591} 95% "),
            "hot pair right-aligned: {row:?}"
        );
        let bar_x = 60 - 1 - "95,000 / 100,000 tok  \u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\u{2591} 95%".chars().count() as u16
            + "95,000 / 100,000 tok  ".chars().count() as u16;
        assert_eq!(
            buf.cell((bar_x, 0)).unwrap().style().fg,
            Some(rgba_color(theme.error)),
            "hot bar in the error tone"
        );

        // No snapshot: the name keeps the whole row, no budget anywhere.
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row = row_text(&buf);
        let cols: Vec<char> = row.chars().collect();
        assert!(
            !row.contains("tok"),
            "no budget before the first report: {row:?}"
        );
        assert_eq!(
            cols[1..5].iter().collect::<String>(),
            "kilo",
            "name keeps its row: {row:?}"
        );
    }

    /// A narrow pane drops to the compact budget (`9.6k/100k 9%`) instead
    /// of overwriting its logo: same three numbers, no bar. A pane that
    /// fits not even the compact pair paints no budget at all.
    #[test]
    fn header_narrow_pane_uses_the_compact_budget() {
        let theme = test_theme();
        let row_text = |buf: &Buffer, w: u16| {
            (0..w)
                .map(|x| {
                    buf.cell((x, 0))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        };

        // 30 columns: the 35-cell full pair cannot fit, the 12-cell
        // compact one does — right-aligned, logo intact.
        let area = Rect::new(0, 0, 30, 10);
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.panes[0].activity.usage = Some((100_000, 9_612));
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row = row_text(&buf, 30);
        let cols: Vec<char> = row.chars().collect();
        let compact: Vec<char> = "9.6k/100k 9%".chars().collect();
        assert_eq!(
            &cols[17..29],
            &compact[..],
            "compact pair right-aligned: {row:?}"
        );
        assert_eq!(cols[29], ' ', "one breathing column at the edge");
        assert_eq!(
            cols[1..5].iter().collect::<String>(),
            "kilo",
            "the logo survives: {row:?}"
        );
        assert_eq!(
            buf.cell((17, 0)).unwrap().style().fg,
            Some(rgba_color(theme.text_muted)),
            "compact counter in the muted tone"
        );
        assert_eq!(
            buf.cell((27, 0)).unwrap().style().fg,
            Some(rgba_color(theme.text_muted)),
            "cool compact meter in the muted tone"
        );
        assert!(
            buf.cell((27, 0))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the compact percentage reads bold too"
        );

        // 12 columns: not even the compact pair fits — name only.
        let area = Rect::new(0, 0, 12, 10);
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.panes[0].activity.usage = Some((100_000, 9_612));
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row = row_text(&buf, 12);
        assert!(!row.contains('%'), "no budget at all: {row:?}");
        assert_eq!(
            &row.chars().collect::<Vec<_>>()[1..5],
            &['k', 'i', 'l', 'o']
        );
    }

    /// The budget never covers the logo: at 40 columns the 35-cell full
    /// pair would fit the pane but NOT beside the 4-cell name, so the
    /// header drops to compact while the name can still be kept.
    #[test]
    fn header_budget_yields_to_the_logo_before_clipping_it() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 40, 10);
        let mut state = AgentsSplitState::new(vec!["kilo".into()]);
        state.panes[0].activity.usage = Some((100_000, 9_612));
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut state, &theme);
        let row: String = (0..40)
            .map(|x| {
                buf.cell((x, 0))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let cols: Vec<char> = row.chars().collect();
        let compact: Vec<char> = "9.6k/100k 9%".chars().collect();
        assert_eq!(&cols[27..39], &compact[..], "compact, not full: {row:?}");
        assert_eq!(
            cols[1..5].iter().collect::<String>(),
            "kilo",
            "the logo is kept whole: {row:?}"
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
        assert_eq!(state.focus_hotkey('2'), Some(1));
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

    /// Issue 2: an internal sub-agent's `fs_edit` must paint its REAL
    /// unified diff inside the pane (the chat's `DiffRenderable` panel,
    /// blitted from the shared cell cache) and label the row with the REAL
    /// tool name — not the category short form. Rendered end-to-end
    /// through `render` so the scroll window, the cache insert and the
    /// blit are all exercised together.
    #[test]
    fn render_pane_paints_the_embedded_edit_diff_with_the_real_tool_name() {
        const DIFF: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,2 +1,3 @@\n fn main() {\n-    old();\n+    new();\n }";
        let theme = test_theme();
        let mut state = AgentsSplitState::new(vec!["cosh".into()]);
        state.set_running(0, true);
        state.apply_event(
            0,
            &SubagentEvent::ToolCall {
                id: "c1".to_string(),
                title: "fs_edit src/main.rs".to_string(),
                kind: cosh_tools::subagent::events::ToolKind::Edit,
                status: ToolCallStatus::InProgress,
                raw_input: Some(serde_json::json!({"path": "src/main.rs"})),
                tool: Some("fs_edit".to_string()),
            },
        );
        state.apply_event(
            0,
            &cosh_tools::subagent::events::SubagentEvent::ToolCallUpdate {
                id: "c1".to_string(),
                status: Some(ToolCallStatus::Completed),
                title: None,
                raw_output: None,
                content: cosh_tools::subagent::events::ToolOutputBlock {
                    text: "edited".to_string(),
                    skipped: 0,
                    diff: Some(cosh_tools::subagent::events::ToolDiffSummary {
                        path: "src/main.rs".to_string(),
                        added: 1,
                        removed: 1,
                        unified: Some(DIFF.to_string()),
                    }),
                },
            },
        );
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
        // The REAL tool name, not the category's short form.
        assert!(
            all.contains("fs_edit src/main.rs"),
            "the tool row must carry the real tool name: {all:?}"
        );
        // The diff panel painted through the shared `DiffRenderable`: the
        // `+`/`-` code lines with the hunk header and the line-number
        // gutter (`2 +     new();` — gutter + sign + code).
        assert!(all.contains("@@ -1,2 +1,3 @@"), "hunk header visible: {all:?}");
        assert!(all.contains("2 +     new();"), "added line visible: {all:?}");
        assert!(all.contains("2 -     old();"), "removed line visible: {all:?}");
        // The panel came from the shared cell cache (inserted this frame).
        assert!(
            state.diff_cells.contains_key("0:diff0"),
            "the EditDiff cell cache entry must exist: keys {:?}",
            state.diff_cells.keys().collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    fn split() -> AgentsSplitState {
        AgentsSplitState::new(vec!["opencode".into(), "kilo".into()])
    }

    /// The Esc walk-back contract: expand (Ctrl+O) → Esc restores the
    /// GRID (mode stays open) → the next Esc parks (leaves to the chat).
    /// The FIRST toggle must consume the Esc and leave the split live.
    #[test]
    fn esc_walks_back_grid_first_then_parks() {
        let mut s = split();
        s.focused = Some(1);
        // Expand (Ctrl+O): the focused pane takes the area.
        assert!(s.toggle_maximized(), "expansion must engage");
        assert_eq!(s.maximized, Some(1));
        // First Esc: `restore_maximized` consumes it and restores the grid —
        // the caller must NOT park on this press.
        assert!(s.restore_maximized(), "the first Esc only restores the grid");
        assert_eq!(s.maximized, None, "grid restored");
        // Second Esc: the grid is up, nothing to restore — `false` hands the
        // key to the caller, whose next branch parks the split (leaves to
        // the chat). The toggle must never be what the Esc runs: with the
        // grid up a toggle would RE-EXPAND and the user could never leave.
        assert!(!s.restore_maximized(), "grid up: the Esc falls through to park");
        assert_eq!(s.maximized, None);
        assert_eq!(s.focused, Some(1), "focus survives the walk-back");
        // Ctrl+O is still a true toggle (expand again from the grid).
        assert!(s.toggle_maximized());
        assert_eq!(s.maximized, Some(1));
        assert!(s.toggle_maximized());
        assert_eq!(s.maximized, None);
    }

    /// A hotkey to another pane ends the expansion (grid returns, target
    /// focused) — one gesture back to the grid.
    #[test]
    fn hotkey_ends_the_expansion() {
        let mut s = split();
        s.focused = Some(0);
        assert!(s.toggle_maximized());
        assert_eq!(s.maximized, Some(0));
        assert_eq!(s.focus_hotkey('2'), Some(1));
        assert_eq!(s.maximized, None, "grid returned");
        assert_eq!(s.focused, Some(1));
    }

    /// The maximized layout hands the pane the full area and the others a
    /// zero rect, index positions preserved.
    #[test]
    fn maximized_layout_preserves_indexes() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 30);
        let rects = compute_layout_with_maximized(3, area, Some(1));
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[1], area);
        assert_eq!(rects[0].width, 0);
        assert_eq!(rects[2].width, 0);
        // Grid fallback for `None`.
        let grid = compute_layout_with_maximized(3, area, None);
        assert!(grid.iter().all(|r| r.width > 0));
    }
}
