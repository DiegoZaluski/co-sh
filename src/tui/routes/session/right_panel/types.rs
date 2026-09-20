/// Maximum number of PTY sessions whose output stays IN MEMORY. Sessions
/// never leave the panel's history (metadata is tiny); older FINISHED
/// sessions have their output spilled to a temp file (the state's private
/// `spill_dir`, removed on drop)
/// and reloaded lazily when displayed again (history navigation).
const MEMORY_KEEP_SESSIONS: usize = 8;

/// Maximum number of characters of output retained per PTY session.
/// Only the TAIL is kept, since the panel renders the most recent output.
const MAX_PTY_OUTPUT_CHARS: usize = 60_000;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cosh_tui::core::lib::rgba::RGBA;

use super::RIGHT_PANEL_WIDTH;

use cosh_tui::core::renderables::markdown::estimate_height;
use ratatui::buffer::{Buffer, Cell};

use crate::types::{Part, Session, ToolStatus};
use crate::util::text_region::{TextRegion, extract_text_in_region};

/// Rehydration limits when the panel is rebuilt from a persisted session
/// (app restart, session switch): only the most recent bash command and the
/// newest few subagent windows PER agent CLI are restored, so opening an
/// old session with a long tool history doesn't flood the panel. Older
/// entries remain visible in the chat transcript's tool boxes.
const REHYDRATE_BASH_KEEP: usize = 1;
const REHYDRATE_SUBAGENT_KEEP_PER_AGENT: usize = 3;

/// Minimum gap between subagent layout rebuilds (heights + rendered body
/// cells). During streaming, chunks arrive far more often than the eye can
/// track; rebuilding the whole accumulated body on EVERY chunk re-parses up
/// to 60k chars per frame and stalls the TUI (especially in debug builds).
/// Rebuilds are coalesced to at most one per interval — frames in between
/// serve the previous cells, stale by less than one interval, which is
/// invisible in a streaming panel.
pub(crate) const SUBAGENT_REBUILD_INTERVAL: Duration = Duration::from_millis(100);

/// Minimum useful height (content rows) of one subagent window. Used as the
/// budget floor so a squeezed section still shows the top-ranked window's
/// header instead of hiding every subagent.
pub(crate) const MIN_WINDOW_ROWS: i32 = 3;

/// Distinguishable, CHEERFUL highlighter-style area colors for subagent
/// CLIs — light pinks, lavenders, baby blues, mints (marca-texto feel). One
/// is drawn ONCE per agent CLI per session (see [`RightPanelState::pick_color`])
/// and never reused by another CLI, so each spawned agent keeps the same
/// visual identity while it runs. Blended over the section background they
/// read as a clear, bright per-agent hue.
const AGENT_PALETTE: [(u8, u8, u8); 12] = [
    (255, 153, 204), // pink highlighter
    (255, 179, 186), // light rose
    (255, 214, 165), // peach
    (255, 236, 139), // pastel yellow
    (177, 240, 134), // light lime
    (134, 239, 172), // mint green
    (110, 231, 183), // teal mint
    (103, 232, 249), // cheerful cyan
    (125, 211, 252), // baby blue
    (147, 197, 253), // periwinkle
    (196, 181, 253), // lavender
    (233, 168, 253), // orchid
];

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

/// Convert the persisted ACP tool result into the text the live panel
/// receives through `HarnessEvent::ToolOutput`. During a live call the panel
/// gets only the streamed text, while the transcript stores the final
/// `SubAgentCallOutput` JSON envelope. Rehydration must remove that envelope
/// so a restart renders the same body as the original live session.
///
/// Internal subagents and older persisted entries store their report as plain
/// text, which deliberately passes through unchanged.
fn subagent_display_output(output: String) -> String {
    serde_json::from_str::<cosh_tools::subagent::SubAgentCallOutput>(&output)
        .map(|result| result.output)
        .unwrap_or(output)
}

/// Parse a plan tool's JSON result into the flat todo list shown in the
/// panel: `{"list": {"items": [{"status", "description"}]}}`.
/// Shared by the live `ToolResult` path (events) and the rehydration from
/// persisted sessions, so both paths always agree. `None` when the output
/// is not a plan result.
pub(crate) fn parse_todo_output(output: &str) -> Option<Vec<TodoItem>> {
    let val = serde_json::from_str::<serde_json::Value>(output).ok()?;
    let items = val.get("list")?.get("items")?.as_array()?;
    Some(
        items
            .iter()
            .map(|item| {
                let status = item
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("pending");
                let description = item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                TodoItem {
                    status: match status {
                        "in_progress" => "in_progress",
                        "completed" => "completed",
                        "cancelled" => "cancelled",
                        _ => "pending",
                    }
                    .to_string(),
                    content: description.to_string(),
                }
            })
            .collect(),
    )
}

/// Wrap one logical line into visual rows of at most `wrap_w` columns,
/// breaking at SPACES with whole-word moves — the same word semantics as
/// the chat's markdown renderer: a word that doesn't fit next to the
/// current content starts a fresh row, and a word wider than the column
/// breaks at character level; a space that would overflow is dropped
/// instead of starting the next row. An empty line yields a single empty
/// row; `wrap_w == 0` yields one row.
pub(crate) fn wrap_chars(line: &str, wrap_w: u16) -> Vec<String> {
    let max = usize::from(wrap_w);
    if max == 0 {
        return vec![line.to_string()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    let mut word = String::new();

    // Place the pending word on the current row when it fits next to it,
    // otherwise start a new row (hard-breaking oversized words).
    fn flush_word(
        rows: &mut Vec<String>,
        cur: &mut String,
        cur_w: &mut usize,
        word: &mut String,
        max: usize,
    ) {
        if word.is_empty() {
            return;
        }
        let word_w = word.chars().count();
        // Whole-word move: any word that doesn't fit next to the current
        // content starts on a fresh row (oversized words included).
        if *cur_w + word_w > max && !cur.is_empty() {
            rows.push(std::mem::take(cur));
            *cur_w = 0;
        }
        if word_w <= max {
            cur.push_str(word);
            *cur_w += word_w;
            word.clear();
            return;
        }
        // The word alone is wider than the whole column: break it at
        // character level so no row exceeds the width.
        for ch in word.drain(..) {
            if *cur_w + 1 > max && !cur.is_empty() {
                rows.push(std::mem::take(cur));
                *cur_w = 0;
            }
            cur.push(ch);
            *cur_w += 1;
        }
    }

    for ch in line.chars() {
        match ch {
            '\n' => {
                flush_word(&mut rows, &mut cur, &mut cur_w, &mut word, max);
                rows.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            ' ' => {
                flush_word(&mut rows, &mut cur, &mut cur_w, &mut word, max);
                if cur_w < max {
                    cur.push(' ');
                    cur_w += 1;
                }
            }
            _ => word.push(ch),
        }
    }
    flush_word(&mut rows, &mut cur, &mut cur_w, &mut word, max);
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }
    rows
}

/// Number of visual rows a logical line occupies at `wrap_w` — derived
/// from [`wrap_chars`] itself so height estimates can never diverge from
/// what the renderer draws.
fn wrap_count(line: &str, wrap_w: u16) -> u16 {
    u16::try_from(wrap_chars(line, wrap_w).len()).unwrap_or(u16::MAX)
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
    /// The area tint (agent color blended over the theme background) the
    /// body was rendered on — a different agent window at the same slot
    /// must re-render with its own tint.
    pub(crate) bg: RGBA,
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
    /// A newer session of the same kind arrived (same agent CLI for
    /// subagents): this entry leaves the DEFAULT live display (the report
    /// was already delivered) and is reachable only through history
    /// navigation. Nothing is ever deleted from the panel.
    pub superseded: bool,
    /// When `true`, `output` is EMPTY and its content lives in the file at
    /// `spilled_path`. Reloaded lazily by [`RightPanelState::session_output`].
    pub spilled: bool,
    pub spilled_path: Option<PathBuf>,
}

impl PtySession {
    pub(crate) fn new(id: String, command: String, workdir: Option<String>) -> Self {
        Self {
            id,
            command,
            output: String::new(),
            workdir,
            status: PtyStatus::Running,
            superseded: false,
            spilled: false,
            spilled_path: None,
        }
    }

    /// The agent CLI of a subagent session (`command` is `subagent: {agent}`).
    pub(crate) fn subagent_agent(&self) -> Option<&str> {
        self.command.strip_prefix("subagent: ")
    }

    pub(crate) fn is_subagent(&self) -> bool {
        self.command.starts_with("subagent:")
    }

    /// Finished sessions are never updated again and can be spilled/hidden.
    pub(crate) fn is_finished(&self) -> bool {
        !matches!(self.status, PtyStatus::Running)
    }
}

/// Identifies which section of the right panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Todo,
    Bash,
    Subagent,
}

/// Keyboard focus within the right panel, set by clicking a section and
/// consumed by the ← / → history keys. Subagent focus is per agent CLI:
/// each queue navigates independently (Kilo never pulls OpenCode entries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelFocus {
    Bash,
    Agent(String),
}

/// Navigation state of one subagent queue (all sessions of ONE agent CLI).
///
/// `index` is anchored to ABSOLUTE queue positions so new sessions never
/// shift what the user selected: `None` = live view (auto follows the
/// newest), `Some(i)` = pinned to `queue[i]`. Pinning at the newest entry
/// behaves like live for auto-switch purposes.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentNav {
    /// Selected queue position (`None` = live).
    pub(crate) index: Option<usize>,
    /// When the user last manually navigated this queue. Drives the
    /// eviction heuristic: when display space must be reclaimed among
    /// PINNED windows, the ones navigated longest ago are hidden first —
    /// the most recently visited is the one the user was last looking at.
    pub(crate) last_nav: Option<Instant>,
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
///
/// NOT `Clone`: it owns the spill directory and cleans it up on `Drop`; a
/// clone would share the path and the first drop would delete the other's
/// spilled files.
#[derive(Debug)]
pub struct RightPanelState {
    /// Current list of TODOs.
    pub todos: Vec<TodoItem>,
    /// Active/completed PTY sessions.
    pub pty_sessions: Vec<PtySession>,
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
    /// Cached bash line buffer (`$ command` + output lines, wrapped to the
    /// section width), keyed to `pty_gen` + `bash_cache_w`.
    bash_buffer_cache: Vec<String>,
    bash_cache_gen: u64,
    /// Width the bash buffer was wrapped at (cache key alongside gen).
    bash_cache_w: u16,
    /// Wrap width of the bash section as of the last layout pass — lets
    /// scroll math (`section_max_scroll`) match the renderer without a
    /// width parameter.
    pub(crate) bash_wrap_w: u16,
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

    // ── History navigation (bash toggle + per-agent queues) ────────
    /// Keyboard focus inside the panel (set by clicking a section). Plain
    /// ←/→ drive the focused slot's history; without focus they keep the
    /// prompt-cursor behavior.
    pub panel_focus: Option<PanelFocus>,
    /// Manual show/hide override (Ctrl+P). `true` keeps the panel hidden
    /// even when it has content and the terminal is wide enough.
    pub user_hidden: bool,
    /// Bash view mode: `false` = live (only the most recent command +
    /// output), `true` = history (ALL commands stacked linearly, exactly
    /// the pre-history rendering).
    pub bash_history_mode: bool,
    /// Per-agent-CLI navigation state for the subagent section.
    agent_navs: HashMap<String, AgentNav>,
    /// Subagent sessions displayed this frame (indices into
    /// `pty_sessions`), resolved by [`Self::resolve_visible_subagents`]
    /// at the start of every render. All subagent render consumers iterate
    /// this set.
    pub(crate) visible_subagents: Vec<usize>,
    /// Screen bands of each rendered subagent window from the last frame
    /// (`session index in pty_sessions`, top, bottom exclusive) — resolves
    /// which AGENT queue a click targeted.
    pub(crate) subagent_window_layouts: Vec<(usize, i32, i32)>,
    /// Area color assigned to each EXTERNAL agent CLI on first sight
    /// (random palette draw, never reused by another CLI). Internal
    /// subagents are NOT in this map: they always use [`Self::host_color`].
    agent_colors: HashMap<String, RGBA>,
    /// The main (cosh) agent's own area color; shared by every internal
    /// subagent. Drawn once per session like the external ones.
    host_color: RGBA,
    /// Entropy counter for the random palette draws (`RandomState` is
    /// re-seeded per instance; this keeps consecutive draws distinct).
    rng_counter: u64,
    /// Directory holding this state instance's spilled outputs. Unique per
    /// instance; removed on drop.
    spill_dir: PathBuf,

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
    /// One region per bash content row (wrapped `bash_buffer` line). Keyed to
    /// `pty_gen` + width via `text_regions_gen`/`text_regions_w`.
    pub(crate) bash_text_regions: Vec<TextRegion>,
    /// One region per subagent content row (header + input + body rows).
    pub(crate) subagent_text_regions: Vec<TextRegion>,
    /// `pty_gen` the text regions were last rebuilt for.
    pub(crate) text_regions_gen: u64,
    /// Wrap width of the SUBAGENT section as of the last layout pass —
    /// lets scroll math (`section_max_scroll`) match the renderer's
    /// `max_w − LEFT_PAD − RIGHT_PAD` without a width parameter.
    pub(crate) subagent_wrap_w: u16,
    /// Wrap width the subagent regions were laid out at.
    pub(crate) text_regions_w: u16,

    // ── Legacy (kept for external consumers) ───────────────────────
    pub scroll_y: i32,
    pub content_height: i32,
    pub visible_height: i32,
}

impl RightPanelState {
    pub fn new() -> Self {
        let mut state = Self {
            todos: Vec::new(),
            pty_sessions: Vec::new(),
            next_pty_id: 0,
            next_activity_id: 1,
            section_activity_order: [0; 3],
            pty_gen: 0,
            subagent_body_cache: Vec::new(),
            bash_buffer_cache: Vec::new(),
            bash_cache_gen: 0,
            bash_cache_w: 0,
            bash_wrap_w: RIGHT_PANEL_WIDTH.saturating_sub(5),
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
            subagent_wrap_w: RIGHT_PANEL_WIDTH.saturating_sub(6),
            text_regions_w: 0,
            panel_focus: None,
            user_hidden: false,
            bash_history_mode: false,
            agent_navs: HashMap::new(),
            visible_subagents: Vec::new(),
            subagent_window_layouts: Vec::new(),
            agent_colors: HashMap::new(),
            host_color: RGBA::from_ints(
                AGENT_PALETTE[0].0,
                AGENT_PALETTE[0].1,
                AGENT_PALETTE[0].2,
                255,
            ),
            rng_counter: 0,
            spill_dir: Self::fresh_spill_dir(),
            scroll_y: 0,
            content_height: 0,
            visible_height: 0,
        };
        // The host agent draws its own area color once per session; every
        // internal subagent inherits it.
        state.host_color = state.pick_color();
        state
    }

    /// Draw a random, not-yet-used palette color for a new agent CLI.
    /// Randomness is native (`RandomState` is seeded uniquely per instance)
    /// plus a monotonic counter; when the palette is exhausted, distinct
    /// variants are derived by mixing toward white/black until unused.
    fn pick_color(&mut self) -> RGBA {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        // Colors already spoken for: every assigned CLI AND the host's own
        // (internal subagents share it, so nobody else may take it).
        let mut used: Vec<RGBA> = self.agent_colors.values().copied().collect();
        used.push(self.host_color);
        self.rng_counter = self.rng_counter.wrapping_add(1);
        let seed = {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(self.rng_counter);
            hasher.write_u64(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64)
                    .unwrap_or(0),
            );
            hasher.finish()
        };

        // First pass: an unused palette entry picked by the seed.
        let free: Vec<usize> = AGENT_PALETTE
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let c = RGBA::from_ints(c.0, c.1, c.2, 255);
                !used.contains(&c)
            })
            .map(|(i, _)| i)
            .collect();
        if let Some(&idx) = free.get((seed % free.len().max(1) as u64) as usize) {
            let (r, g, b) = AGENT_PALETTE[idx];
            return RGBA::from_ints(r, g, b, 255);
        }
        // Palette exhausted: mix the seed-indexed entry toward white/black
        // in alternating steps (delta computed in i32 so BOTH directions
        // produce real variants) until a color nobody holds is found.
        let base = AGENT_PALETTE[(seed % AGENT_PALETTE.len() as u64) as usize];
        for step in 1..=8u32 {
            let t = step as f32 / 9.0;
            let target = if step % 2 == 1 { 255 } else { 0 };
            let mix = |ch: u8| -> u8 {
                let v = i32::from(ch) as f32 + (target - i32::from(ch)) as f32 * t;
                v.round().clamp(0.0, 255.0) as u8
            };
            let candidate = RGBA::from_ints(mix(base.0), mix(base.1), mix(base.2), 255);
            if !used.contains(&candidate) {
                return candidate;
            }
        }
        // Practically unreachable: derive hash-based candidates until one
        // is unused (bounded, then the last derived value wins).
        for j in 0..64u64 {
            let candidate = RGBA::from_ints(
                (seed >> j) as u8,
                (seed >> (8 + j)) as u8,
                (seed >> (16 + j)) as u8,
                255,
            );
            if !used.contains(&candidate) {
                return candidate;
            }
        }
        RGBA::from_ints(seed as u8, (seed >> 8) as u8, (seed >> 16) as u8, 255)
    }

    /// The area color of an agent queue: external CLIs keep their assigned
    /// draw; INTERNAL subagents (empty agent name) always share the main
    /// agent's own color.
    pub fn agent_area_color(&self, agent: &str) -> RGBA {
        if agent.is_empty() {
            self.host_color
        } else {
            self.agent_colors
                .get(agent)
                .copied()
                .unwrap_or(self.host_color)
        }
    }

    /// Unique spill directory for a new state instance:
    /// `<tmp>/cosh/right-panel/<pid>-<nanos>/`. Removed by `Drop`.
    fn fresh_spill_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join("cosh")
            .join("right-panel")
            .join(format!("{}-{nanos}", std::process::id()))
    }

    /// Remove this instance's spilled files. Best-effort: failures (already
    /// gone, permission) are ignored — temp dirs are expendable.
    fn remove_spill_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
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

    // ── History navigation keys ─────────────────────────────────────
    /// ← on the focused slot. Bash toggles history mode; an agent queue
    /// steps one entry back from the current selection (manual navigation
    /// is never overridden by auto-follow).
    pub fn panel_left(&mut self) {
        match self.panel_focus.clone() {
            Some(PanelFocus::Bash) if !self.bash_history_mode => {
                self.bash_history_mode = true;
                self.bump_gen_for_layout();
            }
            Some(PanelFocus::Bash) => {}
            Some(PanelFocus::Agent(agent)) => {
                let queue = self.agent_queue(&agent);
                if queue.len() < 2 {
                    return;
                }
                let nav = self.agent_navs.entry(agent).or_default();
                nav.index = match nav.index {
                    // Live → the entry before the newest.
                    None => Some(queue.len() - 2),
                    Some(i) => Some(i.saturating_sub(1)),
                };
                nav.last_nav = Some(Instant::now());
                self.bump_gen_for_layout();
            }
            None => {}
        }
    }

    /// → on the focused slot. Bash returns to live; an agent queue steps
    /// forward, and reaching the newest entry re-arms auto-follow.
    pub fn panel_right(&mut self) {
        match self.panel_focus.clone() {
            Some(PanelFocus::Bash) if self.bash_history_mode => {
                self.bash_history_mode = false;
                self.bump_gen_for_layout();
            }
            Some(PanelFocus::Bash) => {}
            Some(PanelFocus::Agent(agent)) => {
                let queue = self.agent_queue(&agent);
                let nav = self.agent_navs.entry(agent).or_default();
                let next = match nav.index {
                    // Already live.
                    None => None,
                    Some(i) => {
                        if i + 1 >= queue.len().saturating_sub(1) {
                            // Stepped onto the newest → back to live.
                            None
                        } else {
                            Some(i + 1)
                        }
                    }
                };
                if next != nav.index {
                    nav.index = next;
                    nav.last_nav = Some(Instant::now());
                    self.bump_gen_for_layout();
                }
            }
            None => {}
        }
    }

    /// Alt+← / Alt+→ (and Shift+B/N) switch the keyboard focus across agent
    /// queues (alphabetical order, wrapping). Switching also hands the
    /// DISPLAY to the newly focused queue: its NEWEST entry takes over
    /// immediately (nav reset → auto-follow), replacing whatever was on
    /// screen. This is how HIDDEN queues — those with no window on screen,
    /// unreachable by plain ←/→ which navigate only the focused queue — are
    /// reached. Works from any panel focus.
    pub fn cycle_agent_queue(&mut self, dir: i32) -> bool {
        let mut agents: Vec<String> = self
            .pty_sessions
            .iter()
            .filter_map(|s| s.subagent_agent().map(str::to_string))
            .collect();
        agents.sort();
        agents.dedup();
        if agents.is_empty() {
            return false;
        }
        let cur = match &self.panel_focus {
            Some(PanelFocus::Agent(a)) => agents.iter().position(|x| x == a),
            _ => None,
        };
        let n = agents.len() as i32;
        let next = match cur {
            Some(i) => (i as i32 + dir).rem_euclid(n) as usize,
            // From Bash/no focus: ← enters the FIRST queue alphabetically,
            // → the LAST one.
            None => {
                if dir < 0 {
                    0
                } else {
                    agents.len() - 1
                }
            }
        };
        let next_agent = agents[next].clone();
        // Cycling within a SINGLE queue is a no-op: re-selecting the
        // already-focused queue must not drop its navigation pin.
        if let Some(PanelFocus::Agent(cur)) = &self.panel_focus
            && cur == &next_agent
        {
            return true;
        }
        self.panel_focus = Some(PanelFocus::Agent(next_agent.clone()));
        // The newest entry of the newly focused queue takes over: drop any
        // previous manual navigation (live/auto-follow) and force the layout
        // caches to re-resolve THIS frame so its window swaps in instantly.
        self.agent_navs.remove(&next_agent);
        self.bump_gen_for_layout();
        self.last_subagent_rebuild = Instant::now()
            .checked_sub(self.subagent_rebuild_interval)
            .unwrap_or_else(Instant::now);
        true
    }

    /// Whether the given agent queue is pinned away from its latest entry
    /// (manual navigation active → no auto-follow).
    pub(crate) fn queue_is_pinned(&self, agent: &str) -> bool {
        let len = self.queue_len(agent);
        match self.agent_navs.get(agent).and_then(|n| n.index) {
            None => false,
            Some(i) => i + 1 < len,
        }
    }

    /// The pinned session index (into `pty_sessions`) of an agent queue, if
    /// the user navigated away from live.
    pub(crate) fn pinned_session(&self, agent: &str) -> Option<usize> {
        if !self.queue_is_pinned(agent) {
            return None;
        }
        let queue = self.agent_queue(agent);
        let i = self.agent_navs.get(agent)?.index?;
        queue.get(i).copied()
    }

    /// Invalidate the derived layout caches after a nav change (the visible
    /// window set changes even though outputs did not).
    fn bump_gen_for_layout(&mut self) {
        self.pty_gen = self.pty_gen.wrapping_add(1);
    }

    // ── Subagent dynamic windows ────────────────────────────────────
    /// Separator rows between DISPLAYED subagent windows: a 1-row margin
    /// exists ONLY between consecutive windows. The LAST window leans on
    /// the box's own bottom padding, so a lone or trailing window never
    /// gets doubled bottom padding.
    pub(crate) fn subagent_window_margins(window_count: usize) -> i32 {
        window_count.saturating_sub(1) as i32
    }

    /// Resolve which subagent sessions are DISPLAYED this frame and store
    /// them in `visible_subagents` (display order: chronological).
    ///
    /// Candidates ranked best-first:
    ///   0. The FOCUSED queue's current window (its selected entry while
    ///      navigating, otherwise its newest) — switching focus must hand
    ///      the display to that queue even when space is scarce.
    ///   1. PINNED queues (user navigated there deliberately), most recently
    ///      navigated first — that is where the user was last looking. When
    ///      space must be reclaimed among pinned windows, the ones with the
    ///      OLDEST navigation activity are hidden first.
    ///   2. Live RUNNING sessions (not superseded), newest first.
    ///   3. Live finished sessions (not superseded), newest first.
    ///
    /// A pin suppresses only its queue's FINISHED entries: a live RUNNING
    /// session of a pinned queue still ranks normally, so new work is never
    /// omitted because of a stale pin on an old, no-longer-running window.
    ///
    /// Windows are included greedily while they fit `budget_rows`; the
    /// top-ranked window is always kept (scrolling internally when it alone
    /// exceeds the budget). Superseded sessions never enter the default
    /// display — they are reachable only via navigation.
    pub(crate) fn resolve_visible_subagents(&mut self, wrap_w: u16, budget_rows: i32) {
        // ONE pass over the sessions builds every per-queue view used
        // below (absolute index list + pinned state), instead of the
        // O(N) `queue_*` scans per candidate.
        let mut queues: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in self.pty_sessions.iter().enumerate() {
            if let Some(agent) = s.subagent_agent() {
                queues.entry(agent.to_string()).or_default().push(i);
            }
        }
        // The focused queue's window leads the ranking so a queue switch
        // always swaps the displayed content, even under tight budgets.
        let focused_window: Option<usize> = match &self.panel_focus {
            Some(PanelFocus::Agent(agent)) => {
                let queue = queues.get(agent.as_str());
                match self.agent_navs.get(agent.as_str()).and_then(|n| n.index) {
                    Some(i) => queue.and_then(|q| q.get(i)).copied(),
                    None => queue.and_then(|q| q.last()).copied(),
                }
            }
            _ => None,
        };
        let is_pinned = |agent: &str,
                         navs: &HashMap<String, AgentNav>,
                         queues: &HashMap<String, Vec<usize>>| {
            navs.get(agent)
                .and_then(|n| n.index)
                .is_some_and(|i| i + 1 < queues.get(agent).map_or(0, Vec::len))
        };

        // (rank class, tiebreak, session idx)
        let mut ranked: Vec<(u8, u64, usize)> = Vec::new();
        if let Some(idx) = focused_window {
            ranked.push((0, u64::MAX, idx));
        }

        // 1. Pinned queues — most recent manual navigation first. A pinned
        //    queue contributes exactly ONE window: its selected entry (its
        //    live RUNNING sessions are NOT suppressed — see the live loop
        //    below). The focused queue is already represented above.
        let focused_agent = match &self.panel_focus {
            Some(PanelFocus::Agent(a)) => Some(a.as_str()),
            _ => None,
        };
        let mut pinned: Vec<(Instant, usize)> = self
            .pinned_agents()
            .iter()
            .filter(|agent| focused_agent != Some(agent.as_str()))
            .filter_map(|agent| {
                if !is_pinned(agent, &self.agent_navs, &queues) {
                    return None;
                }
                let nav = self.agent_navs.get(agent)?;
                let idx = nav.index.and_then(|i| queues.get(agent)?.get(i)).copied()?;
                Some((nav.last_nav?, idx))
            })
            .collect();
        pinned.sort_by_key(|p| std::cmp::Reverse(p.0));
        let mut pinned_windows: Vec<usize> = Vec::new();
        for (_, idx) in pinned {
            pinned_windows.push(idx);
            ranked.push((0, 0, idx));
        }

        // 2 + 3. Live candidates, newest first (higher pty index = newer).
        for (idx, s) in self.pty_sessions.iter().enumerate().rev() {
            if s.superseded || !s.is_subagent() {
                continue;
            }
            if Some(idx) == focused_window {
                continue; // already ranked as the focused window
            }
            let agent = s.subagent_agent().unwrap_or("");
            if pinned_windows.contains(&idx) {
                continue; // already ranked as its queue's pinned window
            }
            if is_pinned(agent, &self.agent_navs, &queues) && s.is_finished() {
                continue; // a pin suppresses only finished entries; the
                          // queue's live RUNNING sessions still rank, so
                          // new work never hides behind the pin
            }
            let class = if s.is_finished() { 2 } else { 1 };
            ranked.push((class, idx as u64, idx));
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

        // Pinned windows WILL be displayed — load them so their heights are
        // accurate for the fit (spilled live candidates stay unloaded; they
        // fit as header-only and scroll internally if chosen).
        let pinned_idx: Vec<usize> = ranked
            .iter()
            .filter(|(class, _, _)| *class == 0)
            .map(|&(_, _, i)| i)
            .collect();
        self.ensure_outputs_loaded(&pinned_idx);

        // Greedy fit against the available budget. Every window AFTER the
        // first adds its 1-row separator margin (margins exist only
        // BETWEEN windows — see [`Self::subagent_window_margins`]).
        let rows = self.subagent_section_rows_for_display(wrap_w);
        let mut chosen: Vec<usize> = Vec::new();
        let mut used: i32 = 0;
        for &(_, _, idx) in &ranked {
            let need = i32::from(rows.get(idx).copied().unwrap_or(1)) + !chosen.is_empty() as i32;
            if used + need <= budget_rows || chosen.is_empty() {
                chosen.push(idx);
                used += need;
            }
        }
        chosen.sort_unstable();
        self.visible_subagents = chosen;
    }

    /// Agent CLIs whose queue is currently pinned away from live.
    fn pinned_agents(&self) -> Vec<String> {
        self.agent_navs
            .iter()
            .filter(|(agent, nav)| nav.last_nav.is_some() && self.queue_is_pinned(agent))
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// Rows of ALL subagent sessions aligned with `pty_sessions` indices
    /// (0 for non-subagent entries). Only the sessions currently DISPLAYED
    /// have their spill files reloaded — reloading everything would defeat
    /// the memory window every frame. Unloaded entries contribute a
    /// header-only height for fitting; once a session becomes visible,
    /// [`Self::session_output`] loads it and bumps `pty_gen`, rebuilding
    /// the rows with real content.
    pub(crate) fn subagent_section_rows_for_display(&mut self, wrap_w: u16) -> Vec<u16> {
        let visible = self.visible_subagents.clone();
        self.ensure_outputs_loaded(&visible);
        self.subagent_section_rows(wrap_w);
        let mut out = vec![0u16; self.pty_sessions.len()];
        let mut it = self.subagent_rows_cache.iter();
        for (i, s) in self.pty_sessions.iter().enumerate() {
            if s.is_subagent()
                && let Some(&r) = it.next()
            {
                out[i] = r;
            }
        }
        out
    }

    /// Reload the spill files of the given sessions (if any), so heights and
    /// rendered bodies see real content.
    fn ensure_outputs_loaded(&mut self, idxs: &[usize]) {
        for &i in idxs {
            if self.pty_sessions.get(i).is_some_and(|s| s.spilled) {
                self.session_output(i);
            }
        }
    }

    // ── Section layout (for cursor-based targeting) ──────────────────
    /// Reset the section layout list (call when the panel is hidden).
    pub fn clear_section_layouts(&mut self) {
        self.section_layouts.clear();
        // Stale window bands must never map a click to an agent queue.
        self.subagent_window_layouts.clear();
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

    /// Focus the panel slot at screen row `y` (called on mouse-down):
    /// bash focuses the bash toggle; a subagent window focuses THAT
    /// window's agent queue — each CLI navigates independently. Returns
    /// whether a focusable slot was hit.
    pub fn focus_at(&mut self, y: u16) -> bool {
        let Some(kind) = self.section_at(y) else {
            return false;
        };
        match kind {
            SectionKind::Todo => false,
            SectionKind::Bash => {
                self.panel_focus = Some(PanelFocus::Bash);
                true
            }
            SectionKind::Subagent => {
                let ys = i32::from(y);
                let agent = self
                    .subagent_window_layouts
                    .iter()
                    .find(|&(_, top, bottom)| ys >= *top && ys < *bottom)
                    .and_then(|&(idx, _, _)| {
                        self.pty_sessions
                            .get(idx)
                            .and_then(|s| s.subagent_agent().map(str::to_string))
                    });
                match agent {
                    Some(agent) => {
                        self.panel_focus = Some(PanelFocus::Agent(agent));
                        true
                    }
                    None => false,
                }
            }
        }
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
        } else {
            if !self.is_auto_scrolling {
                self.is_auto_scrolling = true;
                self.auto_scroll_accumulator = 0.0;
            }
            // Mirror the chat pane: the speed is re-derived from the cursor
            // position on EVERY drag move, so pushing the selection deeper
            // into the edge zone accelerates instead of staying at whatever
            // speed happened to be captured when auto-scroll first armed.
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
            SectionKind::Bash => self.bash_buffer(self.bash_wrap_w).len() as i32,
            SectionKind::Subagent => {
                let wrap_w = self.subagent_wrap_w.max(1);
                let rows = self.subagent_section_rows_for_display(wrap_w);
                self.visible_subagents
                    .iter()
                    .map(|&i| i32::from(rows.get(i).copied().unwrap_or(1)))
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

    /// Manual show/hide override (Ctrl+P).
    pub fn toggle_hidden(&mut self) {
        self.user_hidden = !self.user_hidden;
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
        // A new subagent of the same CLI supersedes the previous entries of
        // that queue (the report was delivered); they leave the default
        // display but stay reachable through ← navigation. Bash needs no
        // flag: live shows only the newest, history shows everything.
        if let Some(agent) = command.strip_prefix("subagent: ") {
            let prefix = format!("subagent: {agent}");
            for s in &mut self.pty_sessions {
                if s.command == prefix && s.is_finished() {
                    s.superseded = true;
                }
            }
            // External CLIs draw their area color ONCE (first spawn) and
            // keep it for the whole session; internal subagents (empty
            // agent) share the host color and never enter the draw.
            if !agent.is_empty() && !self.agent_colors.contains_key(agent) {
                let color = self.pick_color();
                self.agent_colors.insert(agent.to_string(), color);
            }
            // A pin whose target was just superseded by this new spawn is
            // stale: that old window left the default display, and keeping
            // the pin would hide this queue's new RUNNING subagent behind
            // an old, no-longer-running entry. Release it so the queue
            // returns to live view (same rule as bash: a new command
            // returns the section to live). A pin on an entry that is still
            // running (concurrent same-CLI sessions) is kept, and the new
            // live session is displayed alongside it.
            let pin_superseded = self
                .agent_navs
                .get(agent)
                .and_then(|nav| nav.index)
                .and_then(|i| self.agent_queue(agent).get(i).copied())
                .is_some_and(|idx| self.pty_sessions[idx].superseded);
            let len = self.queue_len(agent);
            let nav = self.agent_navs.entry(agent.to_string()).or_default();
            if pin_superseded || nav.index.is_none_or(|i| i + 1 >= len) {
                nav.index = None;
            }
        } else {
            // New bash command returns the section to the live view.
            self.bash_history_mode = false;
        }
        self.mark_activity(kind);
        self.pty_sessions
            .push(PtySession::new(id, command, workdir));
        // Bound MEMORY: sessions are kept forever (metadata is tiny), but
        // finished sessions beyond the in-memory window have their output
        // spilled to disk.
        self.pty_gen = self.pty_gen.wrapping_add(1);
        self.spill_old_outputs();
    }

    /// Number of sessions in one agent's queue.
    pub(crate) fn queue_len(&self, agent: &str) -> usize {
        self.pty_sessions
            .iter()
            .filter(|s| s.subagent_agent() == Some(agent))
            .count()
    }

    /// The pinned absolute index of an agent's queue (`None` = live).
    pub(crate) fn queue_nav_index(&self, agent: &str) -> Option<usize> {
        self.agent_navs.get(agent).and_then(|nav| nav.index)
    }

    /// Force a queue's selected index (tests / programmatic pinning).
    #[cfg(test)]
    pub(crate) fn set_queue_index(&mut self, agent: &str, index: usize) {
        let nav = self.agent_navs.entry(agent.to_string()).or_default();
        nav.index = Some(index);
        nav.last_nav = Some(Instant::now());
    }

    /// Absolute indices into `pty_sessions` of one agent's queue (oldest
    /// first). The positions are stable: new sessions append, never shift.
    pub(crate) fn agent_queue(&self, agent: &str) -> Vec<usize> {
        self.pty_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.subagent_agent() == Some(agent))
            .map(|(i, _)| i)
            .collect()
    }

    /// Indices of all bash sessions (oldest first).
    pub(crate) fn bash_queue(&self) -> Vec<usize> {
        self.pty_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_subagent())
            .map(|(i, _)| i)
            .collect()
    }

    /// Output of a session, lazily reloading it from its spill file when it
    /// was evicted from memory.
    pub(crate) fn session_output(&mut self, idx: usize) -> &str {
        let Some(s) = self.pty_sessions.get_mut(idx) else {
            return "";
        };
        if s.spilled
            && let Some(path) = s.spilled_path.clone()
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            s.output = content;
            s.spilled = false;
            self.pty_gen = self.pty_gen.wrapping_add(1);
        }
        &self.pty_sessions[idx].output
    }

    /// Spill the outputs of the oldest FINISHED sessions that no longer fit
    /// the in-memory window. Running sessions always stay in memory.
    fn spill_old_outputs(&mut self) {
        let in_memory = self
            .pty_sessions
            .iter()
            .filter(|s| !s.spilled && !s.output.is_empty())
            .count();
        if in_memory <= MEMORY_KEEP_SESSIONS {
            return;
        }
        let mut excess = in_memory - MEMORY_KEEP_SESSIONS;
        let dir = self.spill_dir.clone();
        for s in &mut self.pty_sessions {
            if excess == 0 {
                break;
            }
            // Oldest-first; running/newest-in-window sessions sit at the end.
            if s.spilled || !s.is_finished() || s.output.is_empty() {
                continue;
            }
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{}.txt", s.id));
            // Retry once: a concurrent removal of the parent dir (another
            // instance dropping) can fail a single write; recreating fixes it.
            if std::fs::write(&path, &s.output).is_err() {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&path, &s.output);
            }
            match std::fs::metadata(&path) {
                Ok(m) if m.len() as usize == s.output.len() => {
                    s.output = String::new();
                    s.spilled = true;
                    s.spilled_path = Some(path);
                    excess -= 1;
                }
                // Best-effort: keep THIS session in memory on failure and
                // keep spilling the remaining ones.
                _ => continue,
            }
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
        // Byte offsets can land inside a multi-byte UTF-8 char (PTY output is
        // arbitrary text); advance to the next char boundary before slicing,
        // otherwise `&output[start..]` panics.
        let mut start = output.len() - MAX_PTY_OUTPUT_CHARS;
        while !output.is_char_boundary(start) {
            start += 1;
        }
        let cut = output[start..].find('\n').map_or(start, |i| start + i + 1);
        output.drain(..cut);
    }

    /// Rebuild the panel content from a persisted session by replaying its
    /// tool parts (plan todos, bash runs, subagent calls), so the panel
    /// survives an app restart or a session switch. Everything restored is
    /// FINISHED — live PTY processes died with the previous app instance —
    /// and bounded (see the `REHYDRATE_*` limits) so a long history doesn't
    /// flood the panel.
    pub fn rehydrate_from_session(&mut self, session: &Session) {
        // ── Todos: the LAST plan tool result wins ──
        for msg in &session.messages {
            for part in &msg.parts {
                let Part::Tool(tp) = part else { continue };
                if !matches!(tp.tool.as_str(), "plan_todo_write") {
                    continue;
                }
                if let Some(output) = &tp.output
                    && let Some(todos) = parse_todo_output(output)
                {
                    self.todos = todos;
                }
            }
        }
        if !self.todos.is_empty() {
            self.mark_activity(SectionKind::Todo);
        }

        // ── PTY sessions: collect bash + subagent calls chronologically ──
        let mut bashes: Vec<(String, String, bool)> = Vec::new();
        // (agent, "→ cosh:" input line, output, failed)
        let mut subs: Vec<(String, Option<String>, String, bool)> = Vec::new();
        for msg in &session.messages {
            for part in &msg.parts {
                let Part::Tool(tp) = part else { continue };
                // No persisted output (crash mid-run) means the process
                // never finished: restored as failed, not silently complete.
                let failed = matches!(tp.status, ToolStatus::Failed(_)) || tp.output.is_none();
                let output = tp.output.clone().unwrap_or_default();
                match tp.tool.as_str() {
                    "bash_run" => {
                        let command = tp
                            .input
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        bashes.push((command, output, failed));
                    }
                    "subagent_call" => {
                        let agent = tp
                            .input
                            .get("agent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = tp
                            .input
                            .get("input")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        subs.push((agent, input, subagent_display_output(output), failed));
                    }
                    _ => {}
                }
            }
        }

        // Trim to the rehydration limits: the newest bash command only, and
        // per agent CLI the newest few windows (older same-agent entries are
        // superseded by the live display anyway).
        if bashes.len() > REHYDRATE_BASH_KEEP {
            bashes.drain(..bashes.len() - REHYDRATE_BASH_KEEP);
        }
        let kept_subs: Vec<(String, Option<String>, String, bool)> = subs
            .iter()
            .enumerate()
            .filter(|(i, (agent, ..))| {
                subs[i + 1..]
                    .iter()
                    .filter(|(a, ..): &&(String, Option<String>, String, bool)| a == agent)
                    .count()
                    < REHYDRATE_SUBAGENT_KEEP_PER_AGENT
            })
            .map(|(_, entry)| entry.clone())
            .collect();

        for (command, output, failed) in bashes {
            self.replay_pty(command, None, output, failed);
        }
        for (agent, input, output, failed) in kept_subs {
            let command = format!("subagent: {agent}");
            // Same contract as the live path (events): an EMPTY input
            // message never draws a "→ cosh:" line.
            let input_line = input
                .filter(|msg| !msg.is_empty())
                .map(|msg| format!("→ cosh: {msg}\n"));
            self.replay_pty(command, input_line, output, failed);
        }
    }

    /// Append one restored PTY session through the live `start_pty` path
    /// (so superseding, per-agent colors and queue bookkeeping match a live
    /// run exactly), then fill its final output and terminal status.
    fn replay_pty(
        &mut self,
        command: String,
        input_line: Option<String>,
        output: String,
        failed: bool,
    ) {
        self.start_pty(command, None);
        let Some(session) = self.pty_sessions.last_mut() else {
            return;
        };
        if let Some(line) = input_line {
            session.output.push_str(&line);
        }
        session.output.push_str(&output);
        Self::truncate_output(&mut session.output);
        session.status = if failed {
            PtyStatus::Failed
        } else {
            PtyStatus::Completed
        };
        self.pty_gen = self.pty_gen.wrapping_add(1);
    }

    /// Derived line buffer of the bash PTY sessions (`$ command` + output
    /// lines), WRAPPED to `wrap_w` columns: long words/lines break onto the
    /// following rows instead of disappearing past the box edge. LIVE mode
    /// (default) renders ONLY the most recent bash session — one command +
    /// its output per view; HISTORY mode (←) shows ALL commands stacked
    /// linearly, exactly like the pre-history panel. Cached until the next
    /// PTY mutation or mode/width change: rebuilding the entire accumulated
    /// output on every frame was the dominant per-frame cost of the right
    /// panel as a session grew long.
    pub(crate) fn bash_buffer(&mut self, wrap_w: u16) -> &[String] {
        let width_changed = wrap_w != self.bash_cache_w;
        if self.pty_gen != self.bash_cache_gen || width_changed {
            self.bash_buffer_cache.clear();
            let queue = self.bash_queue();
            let shown: &[usize] = if self.bash_history_mode {
                &queue
            } else {
                match queue.last() {
                    Some(last) => std::slice::from_ref(last),
                    None => &[],
                }
            };
            for &i in shown {
                let command = self.pty_sessions[i].command.clone();
                // Header: $ command (simulating a shell prompt)
                let header = format!("$ {command}");
                self.bash_buffer_cache.extend(wrap_chars(&header, wrap_w));
                let output = self.session_output(i).to_string();
                for line in output.lines() {
                    self.bash_buffer_cache.extend(wrap_chars(line, wrap_w));
                }
            }
            self.bash_cache_gen = self.pty_gen;
            self.bash_cache_w = wrap_w;
        }
        &self.bash_buffer_cache
    }

    /// Derived line buffer of ALL subagent sessions (command header +
    /// output lines). WARNING: reloads every spilled output when the cache
    /// rebuilds — do NOT wire this into the render path (the memory window
    /// would be defeated); it exists for diagnostics/tests.
    /// Same-agent entries are superseded, never removed.
    /// Cached until the next PTY mutation (see [`Self::bash_buffer`]).
    pub(crate) fn subagent_buffer(&mut self) -> &[String] {
        if self.pty_gen != self.subagent_cache_gen {
            self.subagent_buffer_cache.clear();
            for i in 0..self.pty_sessions.len() {
                let command = self.pty_sessions[i].command.clone();
                if !command.starts_with("subagent:") {
                    continue;
                }
                // Header: the command line (e.g., "subagent: opencode")
                self.subagent_buffer_cache.push(command);
                let output = self.session_output(i).to_string();
                for line in output.lines() {
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
                // Command header AND input line wrap like any text: long
                // ones occupy multiple visual rows (the renderer splits
                // them the same way).
                let mut rows: u16 = wrap_count(&pty.command, wrap_w);
                if let Some(input) = input {
                    rows = rows.saturating_add(wrap_count(input, wrap_w));
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

impl Drop for RightPanelState {
    fn drop(&mut self) {
        // Session switched or app exited: remove this instance's spilled
        // outputs. Best-effort. NOTE: only THIS instance's directory is
        // removed — pruning the shared parents would race with other
        // instances creating their own directories (parallel tests, two
        // app processes), making their writes fail sporadically.
        Self::remove_spill_dir(&self.spill_dir);
    }
}

/// Palette entries for tests (light/vivid property checks).
#[cfg(test)]
pub(crate) fn palette_entries() -> Vec<(u8, u8, u8)> {
    AGENT_PALETTE.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: wrapping must break at WORD boundaries — a line never
    /// cuts mid-word when spaces allow the move, matching the chat's
    /// markdown renderer semantics. A space kept before a forced break
    /// trails the row invisibly (same as the markdown output).
    #[test]
    fn wrap_chars_breaks_at_word_boundaries() {
        // "report" doesn't fit next to "kilo " → moves whole to row 2.
        assert_eq!(wrap_chars("kilo report", 8), vec!["kilo ", "report"]);
        // Multiple words pack greedily up to the width.
        assert_eq!(wrap_chars("aa bb cc dd", 8), vec!["aa bb cc", "dd"]);
    }

    /// A word WIDER than the column cannot fit anywhere: it starts on its
    /// own row and hard-breaks at character level so no row exceeds the
    /// width (markdown parity).
    #[test]
    fn wrap_chars_hard_breaks_oversized_words() {
        assert_eq!(wrap_chars("abcdef", 4), vec!["abcd", "ef"]);
        // Oversized word after content: fresh row, then char-level breaks.
        assert_eq!(wrap_chars("ok abcdefgh", 4), vec!["ok ", "abcd", "efgh"]);
        // Bash header shape: prefix alone, then the oversized run breaks.
        assert_eq!(
            wrap_chars(&format!("$ {}", "a".repeat(30)), 10),
            vec!["$ ", "aaaaaaaaaa", "aaaaaaaaaa", "aaaaaaaaaa"]
        );
        // Discriminates word-wrap from char-chunking ("aa bb|b cc |dd").
        assert_eq!(wrap_chars("aa bbb cc dd", 5), vec!["aa ", "bbb ", "cc dd"]);
    }

    /// Edge contracts preserved: empty line → one empty row; width 0 → one
    /// row; `wrap_count` always agrees with `wrap_chars` (estimates ==
    /// rendering), including multi-byte characters.
    #[test]
    fn wrap_chars_edges_and_count_agreement() {
        assert_eq!(wrap_chars("", 10), vec![""]);
        assert_eq!(wrap_chars("qualquer coisa", 0), vec!["qualquer coisa"]);
        for line in [
            "coração pinga e anda".to_string(),
            "→ cosh: ação é útil".to_string(),
            "x".repeat(200),
            "ab ".repeat(40),
        ] {
            for w in [1u16, 5, 12, 48] {
                let rows = wrap_chars(&line, w);
                assert_eq!(
                    u16::try_from(rows.len()).unwrap(),
                    wrap_count(&line, w),
                    "count divergence for {line:?} at width {w}"
                );
                // Every visual row respects the width…
                assert!(
                    rows.iter().all(|r| r.chars().count() <= usize::from(w)),
                    "row overflow for {line:?} at width {w}: {rows:?}"
                );
                // …and rejoining rows recovers the content (spaces may be
                // dropped only where a break happened). Words wider than
                // the column legitimately hard-break, so word identity is
                // only asserted when every word fits.
                let rejoined: String = rows.join(" ");
                let original_words: Vec<&str> = line.split_whitespace().collect();
                if original_words
                    .iter()
                    .all(|wd| wd.chars().count() <= usize::from(w))
                {
                    assert_eq!(
                        rejoined.split_whitespace().collect::<Vec<_>>(),
                        original_words,
                        "word loss for {line:?} at width {w}"
                    );
                }
            }
        }
    }

    /// Regression: `truncate_output` cuts at a BYTE offset (`len - cap`).
    /// When the output contains multi-byte UTF-8 (arrows, accents, emoji —
    /// common in bash/subagent output), that offset can land inside a char
    /// and slicing panicked with "byte index is not a char boundary". The
    /// cut must advance to the next char boundary instead.
    #[test]
    fn truncate_output_handles_multibyte_output_without_panic() {
        let mut state = RightPanelState::new();
        state.start_pty("cmd".to_string(), None);
        // 30000 × "é" (2 bytes) + "x" = 60001 bytes > 60_000: the naive byte
        // offset start = 1 lands inside the first 'é'.
        state.update_last_pty("é".repeat(30_000) + "x");
        assert!(state.pty_sessions[0].output.len() <= MAX_PTY_OUTPUT_CHARS);

        // Same for subagent output with the "→ cosh:" prefix.
        state.complete_last_pty(String::new());
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty(format!("→ cosh: olá\n{}", "→".repeat(30_001)));
        assert!(state.pty_sessions[1].output.len() <= MAX_PTY_OUTPUT_CHARS);
    }

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

    // ── Rehydration from a persisted session ─────────────────────────

    use crate::types::{Message, MessageRole, Part, Session, ToolPart, ToolStatus};

    fn tool_part(tool: &str, input: serde_json::Value, output: Option<String>) -> Part {
        Part::Tool(ToolPart {
            tool: tool.to_string(),
            input,
            output,
            status: ToolStatus::Completed,
            tool_call_id: None,
            is_start: false,
            is_streaming: false,
            cached_line_count: None,
            lsp_notes: None,
        })
    }

    fn session_with(parts: Vec<Part>) -> Session {
        Session {
            id: "s".to_string(),
            title: "s".to_string(),
            messages: vec![Message {
                id: "msg-0".to_string(),
                role: MessageRole::Assistant,
                parts,
                created_at: 0,
                agent: None,
                model: None,
            }],
            created_at: 0,
            title_generated: false,
            provider: None,
            model: None,
            reasoning: None,
            ctx_ids: Default::default(),
        }
    }

    fn todo_output_json(entries: &[(&str, &str)]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(status, description)| {
                format!(r#"{{"status": "{status}", "description": "{description}"}}"#)
            })
            .collect();
        format!(r#"{{"list": {{"items": [{}]}}}}"#, items.join(","))
    }

    #[test]
    fn rehydrate_restores_todos_from_the_last_plan_result() {
        let mut state = RightPanelState::new();
        let session = session_with(vec![
            tool_part(
                "plan_todo_write",
                serde_json::json!({}),
                Some(todo_output_json(&[("pending", "old item")])),
            ),
            tool_part(
                "plan_todo_write",
                serde_json::json!({}),
                Some(todo_output_json(&[
                    ("in_progress", "first"),
                    ("completed", "second"),
                ])),
            ),
        ]);

        state.rehydrate_from_session(&session);

        assert_eq!(state.todos.len(), 2);
        assert_eq!(state.todos[0].content, "first");
        assert_eq!(state.todos[0].status, "in_progress");
        assert_eq!(state.todos[1].status, "completed");
    }

    #[test]
    fn rehydrate_keeps_only_the_most_recent_bash_command() {
        let mut state = RightPanelState::new();
        let session = session_with(vec![
            tool_part(
                "bash_run",
                serde_json::json!({"command": "first"}),
                Some("out1".to_string()),
            ),
            tool_part(
                "bash_run",
                serde_json::json!({"command": "second"}),
                Some("out2".to_string()),
            ),
        ]);

        state.rehydrate_from_session(&session);

        assert_eq!(state.pty_sessions.len(), 1);
        assert_eq!(state.pty_sessions[0].command, "second");
        assert_eq!(state.pty_sessions[0].output, "out2");
        assert_eq!(state.pty_sessions[0].status, PtyStatus::Completed);
    }

    #[test]
    fn rehydrate_marks_crashed_running_tools_as_failed() {
        let mut state = RightPanelState::new();
        let mut part = tool_part(
            "bash_run",
            serde_json::json!({"command": "never finished"}),
            None,
        );
        if let Part::Tool(tp) = &mut part {
            tp.status = ToolStatus::Running;
        }
        let session = session_with(vec![part]);

        state.rehydrate_from_session(&session);

        assert_eq!(state.pty_sessions[0].status, PtyStatus::Failed);
    }

    #[test]
    fn rehydrate_keeps_last_windows_per_agent_and_supersedes_the_rest() {
        let mut state = RightPanelState::new();
        let mut parts = Vec::new();
        for i in 0..5 {
            parts.push(tool_part(
                "subagent_call",
                serde_json::json!({"agent": "kilo", "input": format!("task {i}")}),
                Some(format!("report {i}")),
            ));
        }
        parts.push(tool_part(
            "subagent_call",
            serde_json::json!({"agent": "opencode", "input": "other task"}),
            Some("other report".to_string()),
        ));
        let session = session_with(parts);

        state.rehydrate_from_session(&session);

        let kilos: Vec<_> = state
            .pty_sessions
            .iter()
            .filter(|s| s.subagent_agent() == Some("kilo"))
            .collect();
        let opencodes: Vec<_> = state
            .pty_sessions
            .iter()
            .filter(|s| s.subagent_agent() == Some("opencode"))
            .collect();
        // Per-agent limit: 3 of 5 kilo windows kept, opencode untouched.
        assert_eq!(kilos.len(), 3);
        assert_eq!(opencodes.len(), 1);
        // The kept ones are the NEWEST of each queue.
        assert!(kilos[0].output.contains("report 2"));
        assert!(kilos[2].output.contains("report 4"));
        assert!(kilos[0].superseded);
        assert!(!kilos[2].superseded);
        // The "→ cosh:" input line is reconstructed as the first line.
        assert!(opencodes[0].output.starts_with("→ cosh: other task\n"));
        assert_eq!(opencodes[0].status, PtyStatus::Completed);
    }

    #[test]
    fn rehydrate_unwraps_the_persisted_acp_output_envelope() {
        let mut state = RightPanelState::new();
        let persisted = serde_json::json!({
            "output": "Oi! Como posso ajudar?",
            "stop_reason": "EndTurn"
        })
        .to_string();
        let session = session_with(vec![tool_part(
            "subagent_call",
            serde_json::json!({"agent": "opencode", "input": "oi"}),
            Some(persisted),
        )]);

        state.rehydrate_from_session(&session);

        assert_eq!(
            state.pty_sessions[0].output,
            "→ cosh: oi\nOi! Como posso ajudar?"
        );
        assert!(!state.pty_sessions[0].output.contains("stop_reason"));
    }

    #[test]
    fn user_hidden_overrides_should_show_right_panel() {
        use crate::routes::session::right_panel::should_show_right_panel;

        let mut state = RightPanelState::new();
        state.start_pty("echo hi".to_string(), None);
        assert!(should_show_right_panel(120, &state));

        state.toggle_hidden();
        assert!(state.user_hidden);
        assert!(!should_show_right_panel(120, &state));

        state.toggle_hidden();
        assert!(should_show_right_panel(120, &state));
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

    /// Sessions are NEVER evicted (history stays navigable); beyond the
    /// in-memory window the oldest FINISHED outputs are spilled to disk and
    /// reloaded lazily. Running sessions always stay in memory.
    #[test]
    fn pty_sessions_spill_outputs_beyond_memory_window() {
        let mut state = RightPanelState::new();
        for i in 0..(MEMORY_KEEP_SESSIONS + 5) {
            state.start_pty(format!("cmd{i}"), None);
            state.complete_last_pty(format!("out{i}"));
        }
        // Nothing is deleted: every command remains in history.
        assert_eq!(state.pty_sessions.len(), MEMORY_KEEP_SESSIONS + 5);
        assert!(state.pty_sessions.iter().any(|p| p.command == "cmd0"));

        let spilled = state.pty_sessions.iter().filter(|p| p.spilled).count();
        assert!(
            (1..=5).contains(&spilled),
            "oldest finished outputs moved to disk (got {spilled})"
        );
        assert!(
            !state.pty_sessions.last().unwrap().spilled,
            "the newest session stays in memory"
        );

        // Lazy reload through the accessor restores content transparently.
        assert_eq!(state.session_output(0), "out0");
        assert!(!state.pty_sessions[0].spilled);

        // A RUNNING session is never spilled even when it is old.
        for i in 0..(MEMORY_KEEP_SESSIONS + 3) {
            state.start_pty(format!("done{i}"), None);
            state.complete_last_pty("ok".to_string());
        }
        state.start_pty("running".to_string(), None);
        state.update_last_pty("streaming...".to_string());
        assert!(!state.pty_sessions.last().unwrap().spilled);
    }

    /// Bash LIVE mode shows only the most recent command + output; HISTORY
    /// mode (←) stacks all commands exactly like the pre-history panel.
    /// A new command always returns to live.
    #[test]
    fn bash_live_and_history_modes() {
        let mut state = RightPanelState::new();
        state.start_pty("echo one".to_string(), None);
        state.complete_last_pty("one".to_string());
        state.start_pty("echo two".to_string(), None);
        state.complete_last_pty("two".to_string());

        assert!(!state.bash_history_mode);
        assert_eq!(
            state.bash_buffer(80),
            &["$ echo two".to_string(), "two".to_string()],
            "live shows only the latest command"
        );

        state.panel_focus = Some(PanelFocus::Bash);
        state.panel_left();
        assert!(state.bash_history_mode);
        assert_eq!(
            state.bash_buffer(80),
            &[
                "$ echo one".to_string(),
                "one".to_string(),
                "$ echo two".to_string(),
                "two".to_string(),
            ],
            "history stacks all commands linearly"
        );

        state.panel_right();
        assert!(!state.bash_history_mode, "→ returns to live");

        // A new command resets to live even if history was active.
        state.panel_left();
        state.start_pty("echo three".to_string(), None);
        assert!(!state.bash_history_mode);
        assert_eq!(
            state.bash_buffer(80).first(),
            Some(&"$ echo three".to_string())
        );
    }

    /// Spill reload is LAZY per DISPLAYED window: rendering frames must not
    /// bulk-reload every spilled output (that would defeat the memory
    /// window and cause disk churn), but a window the user navigates to IS
    /// reloaded.
    #[test]
    fn spill_reload_is_limited_to_displayed_windows() {
        let mut state = RightPanelState::new();
        let n = MEMORY_KEEP_SESSIONS + 4;
        for i in 0..n {
            state.start_pty("subagent: kilo".to_string(), None);
            state.complete_last_pty(format!("report {i}\nsecond line\n"));
        }
        state.subagent_rebuild_interval = Duration::ZERO;
        state.text_regions_w = 20;
        assert!(
            state.pty_sessions.iter().any(|s| s.spilled),
            "oldest outputs were spilled"
        );

        // Live view shows only the newest (older same-CLI entries are
        // superseded): repeated render passes must keep them on disk.
        state.resolve_visible_subagents(20, 6);
        for _ in 0..2 {
            let _ = state.subagent_section_rows_for_display(20);
        }
        let in_memory = state
            .pty_sessions
            .iter()
            .filter(|s| !s.spilled && !s.output.is_empty())
            .count();
        assert!(
            in_memory <= MEMORY_KEEP_SESSIONS + 1,
            "memory window respected across frames (got {in_memory})"
        );
        assert!(state.pty_sessions[0].spilled, "hidden entry stays spilled");

        // Navigating to an old SPILLED entry loads exactly that window.
        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.panel_left();
        if let Some(nav) = state.agent_navs.get_mut("kilo") {
            nav.index = Some(1); // absolute queue position 1 (spilled)
        }
        state.resolve_visible_subagents(20, 6);
        let _ = state.subagent_section_rows_for_display(20);
        let target = state.pinned_session("kilo").unwrap();
        assert_eq!(target, 1);
        assert!(
            !state.pty_sessions[target].spilled,
            "displayed window reloaded"
        );
        assert!(
            state.pty_sessions[0].spilled || target == 0,
            "other hidden entries stay spilled"
        );
    }

    /// Long bash lines wrap onto following visual rows at the section
    /// width instead of being truncated away; the header wraps too.
    #[test]
    fn bash_buffer_wraps_long_lines() {
        let mut state = RightPanelState::new();
        let long_cmd = "a".repeat(30);
        state.start_pty(long_cmd, None);
        state.complete_last_pty("short out\n".to_string());

        // Width 10: "$ " + 30 chars → word semantics: the oversized run
        // starts on its own row and breaks per char → 4 header rows.
        let buf = state.bash_buffer(10).to_vec();
        assert_eq!(buf.len(), 4 + 1, "header wraps, output fits one row");
        assert_eq!(buf[0], "$ ");
        assert_eq!(buf[3].chars().count(), 10);

        // Same content, wider box → fewer rows (width is part of the key).
        assert!(state.bash_buffer(40).len() < buf.len());
    }

    // ── Per-CLI area colors ─────────────────────────────────────────

    /// Every external agent CLI draws its area color ONCE and keeps it for
    /// the whole session; no two CLIs ever share a color; internal
    /// subagents (empty agent name) share the HOST color instead.
    #[test]
    fn agent_area_colors_are_unique_stable_and_host_shared() {
        let mut state = RightPanelState::new();
        let host = state.agent_area_color("");

        for agent in ["opencode", "clint", "kilo", "codex"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty("report\n".to_string());
        }
        // Re-spawn the same CLI: same color, still unique across CLIs.
        state.start_pty("subagent: opencode".to_string(), None);

        let opencode = state.agent_area_color("opencode");
        assert_eq!(
            state.agent_area_color("opencode"),
            opencode,
            "same CLI keeps its color across spawns"
        );

        let mut all: Vec<RGBA> = vec![host];
        for agent in ["opencode", "clint", "kilo", "codex"] {
            let c = state.agent_area_color(agent);
            assert!(!all.contains(&c), "{agent} color collides: {c:?}");
            all.push(c);
        }
        assert_eq!(
            state.agent_area_color(""),
            host,
            "internal subagents always share the HOST color"
        );
    }

    /// The whole palette can be consumed without panic or collision; after
    /// exhaustion, derived colors are still unique.
    #[test]
    fn color_draw_survives_palette_exhaustion() {
        let mut state = RightPanelState::new();
        for i in 0..AGENT_PALETTE.len() + 4 {
            state.start_pty(format!("subagent: agent{i}"), None);
        }
        let mut seen: Vec<RGBA> = vec![state.agent_area_color("")];
        for i in 0..AGENT_PALETTE.len() + 4 {
            let c = state.agent_area_color(&format!("agent{i}"));
            assert!(
                !seen.contains(&c),
                "agent{i} reused a color: {c:?} in {seen:?}"
            );
            seen.push(c);
        }
    }

    /// The subagent rows account for wrapped command-header and input
    /// lines, so the box never clips a window whose input is long.
    #[test]
    fn subagent_rows_include_wrapped_header_and_input() {
        let mut state = RightPanelState::new();
        state.subagent_rebuild_interval = Duration::ZERO;
        state.start_pty("subagent: opencode".to_string(), None);
        let long_input = format!("→ cosh: {}\n", "x".repeat(50));
        state.update_last_pty(long_input); // no body yet

        // Width 20: 18-char header → 1 row; input = "→ cosh: " prefix row
        // + the oversized x-run hard-broken into 20/20/10 → 4 rows.
        assert_eq!(state.subagent_section_rows(20), &[1 + 4]);
        assert_eq!(state.subagent_section_rows(80), &[2]);
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

        let first = state.bash_buffer(80).to_vec();
        assert_eq!(first, vec!["$ echo hi".to_string(), "hi".to_string()]);

        // No mutation → the cached slice is served unchanged.
        assert_eq!(state.bash_buffer(80), first);

        // Mutation bumps the generation → the cache is rebuilt with new output.
        state.start_pty("echo yo".to_string(), None);
        state.complete_last_pty("yo".to_string());
        // LIVE mode shows only the newest; history mode stacks everything.
        assert_eq!(
            state.bash_buffer(80),
            vec!["$ echo yo".to_string(), "yo".to_string()]
        );
        state.panel_focus = Some(PanelFocus::Bash);
        state.panel_left();
        assert_eq!(
            state.bash_buffer(80),
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

        assert_eq!(state.bash_buffer(80).len(), 2);
        assert_eq!(
            state.subagent_buffer().to_vec(),
            vec!["subagent: opencode".to_string(), "agent output".to_string()]
        );
    }

    /// A new session of the same agent CLI supersedes the old one (it
    /// leaves the default display), while other CLIs coexist. Superseded
    /// entries stay in memory and are reachable via ← navigation.
    #[test]
    fn same_cli_supersedes_but_stays_navigable() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: opencode".to_string(), None);
        state.complete_last_pty("report v1".to_string());
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("kilo report".to_string());

        // Same CLI again → the old entry is superseded, nothing is deleted.
        state.start_pty("subagent: opencode".to_string(), None);
        state.update_last_pty("report v2".to_string());
        state.complete_last_pty(String::new());

        assert_eq!(state.pty_sessions.len(), 3, "nothing deleted");
        assert!(state.pty_sessions[0].superseded);
        assert!(!state.pty_sessions[1].superseded, "other CLI untouched");

        let visible = {
            state.subagent_rebuild_interval = Duration::ZERO;
            state.text_regions_w = 38;
            state.resolve_visible_subagents(38, 1000);
            state.visible_subagents.clone()
        };
        // Live shows the newest opencode + the kilo session (plenty of space).
        assert_eq!(visible.len(), 2);
        assert_eq!(visible, vec![1, 2]);

        // ← on the opencode queue reaches the superseded report.
        state.panel_focus = Some(PanelFocus::Agent("opencode".to_string()));
        state.panel_left();
        assert_eq!(state.pinned_session("opencode"), Some(0));

        // → → returns to live (auto-follow re-armed).
        state.panel_right(); // back to newest (index 2)
        state.panel_right(); // newest → live
        assert!(!state.queue_is_pinned("opencode"));
    }

    /// Auto-follow: sessions of OTHER queues never move a manual selection,
    /// but the same-CLI spawn that SUPERSEDES the pinned entry releases the
    /// pin — the new RUNNING session must surface, not stay hidden behind a
    /// stale pin on an old, no-longer-running window.
    #[test]
    fn navigation_pins_queue_against_auto_follow() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("v1".to_string());
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("v2".to_string());

        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.panel_left(); // pin at v1 (queue [v1, v2], absolute index 0)
        assert!(state.queue_is_pinned("kilo"));

        // A session of ANOTHER queue arrives → selection stays put.
        state.start_pty("subagent: opencode".to_string(), None);
        state.complete_last_pty("other report".to_string());
        assert_eq!(
            state.pinned_session("kilo"),
            Some(0),
            "other queues never move the manual selection"
        );

        // The kilo spawn that supersedes the pinned v1 releases the pin:
        // v3 is running and takes the display (live view re-armed).
        state.start_pty("subagent: kilo".to_string(), None);
        state.update_last_pty("v3 streaming".to_string());
        assert!(
            !state.queue_is_pinned("kilo"),
            "a pin on a superseded entry is released: live work must surface"
        );
        assert!(state.pinned_session("kilo").is_none());
    }

    /// REGRESSION: a queue's pin must never hide its live RUNNING sessions —
    /// they rank as normal live candidates and are displayed alongside the
    /// pinned window; only finished entries yield to the pin.
    #[test]
    fn pinned_queue_still_displays_its_running_sessions() {
        // s0 stays RUNNING; s1 runs to completion.
        let mut state = RightPanelState::new();
        state.start_pty("subagent: kilo".to_string(), None);
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("v1 done".to_string());
        assert!(!state.pty_sessions[0].is_finished());
        assert!(state.pty_sessions[1].is_finished());

        // Pin the queue on the still-running s0.
        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.panel_left();
        assert!(state.queue_is_pinned("kilo"));

        // A new kilo session spawns (running): the pin survives (its target
        // is not superseded) and the new RUNNING session is displayed
        // alongside the pinned window instead of being omitted.
        state.start_pty("subagent: kilo".to_string(), None);
        state.update_last_pty("v2 streaming".to_string());
        assert!(state.queue_is_pinned("kilo"), "running pin target survives");

        state.subagent_rebuild_interval = Duration::ZERO;
        state.text_regions_w = 38;
        state.resolve_visible_subagents(38, 1000);
        assert_eq!(
            state.visible_subagents,
            vec![0, 2],
            "pinned window + new RUNNING session, nothing omitted"
        );
    }

    /// When space is reclaimed among PINNED windows, the ones with the
    /// OLDEST manual navigation are hidden first — the most recently
    /// visited is the one the user was last looking at.
    #[test]
    fn pinned_eviction_prefers_most_recently_navigated() {
        let mut state = RightPanelState::new();
        for agent in ["a", "b", "c", "d"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} body\nline2\nline3\n"));
        }
        state.subagent_rebuild_interval = Duration::ZERO;
        state.text_regions_w = 20;

        // User navigates back through ALL queues.
        for agent in ["a", "b", "c", "d"] {
            state.panel_focus = Some(PanelFocus::Agent(agent.to_string()));
            state.panel_left();
        }
        // Deterministic recency: a oldest … d most recent (no sleeps —
        // equal Instants would fall back to HashMap iteration order).
        let base = Instant::now();
        for (k, agent) in ["a", "b", "c", "d"].iter().enumerate() {
            if let Some(nav) = state.agent_navs.get_mut(*agent) {
                nav.last_nav = Some(base + Duration::from_millis(k as u64 * 1_000));
            }
        }

        // Tiny budget: only ONE window fits.
        state.resolve_visible_subagents(20, 4);
        assert_eq!(state.visible_subagents.len(), 1);
        // Queue d was navigated last → its window survives.
        let kept = state.pty_sessions[state.visible_subagents[0]]
            .command
            .clone();
        assert_eq!(kept, "subagent: d", "most recently navigated wins");
    }

    /// Alt+← / Alt+→ cycle focus across agent queues — including HIDDEN
    /// queues that have no window to click — without ever mixing entries
    /// from different CLIs into one queue's navigation.
    #[test]
    fn alt_arrows_cycle_agent_queues() {
        let mut state = RightPanelState::new();
        for agent in ["kilo", "opencode"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} report\n"));
        }
        state.panel_focus = Some(PanelFocus::Agent("opencode".to_string()));

        assert!(state.cycle_agent_queue(-1));
        assert_eq!(
            state.panel_focus,
            Some(PanelFocus::Agent("kilo".to_string())),
            "Alt+← reaches the other queue"
        );
        assert!(state.cycle_agent_queue(1));
        assert_eq!(
            state.panel_focus,
            Some(PanelFocus::Agent("opencode".to_string()))
        );

        // Plain arrows still navigate ONLY the focused CLI's queue.
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("kilo report v2\n".to_string());
        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.panel_left();
        let kilo_q = state.agent_queue("kilo");
        assert_eq!(state.pinned_session("kilo"), Some(kilo_q[0]));
        // The other CLI's queue is untouched (not pinned, not superseded).
        assert!(state.pinned_session("opencode").is_none());
        assert!(
            state
                .agent_queue("opencode")
                .iter()
                .all(|&i| !state.pty_sessions[i].superseded)
        );
    }

    /// REGRESSION: switching queues (Shift+B/N / Alt+arrows) must hand the
    /// DISPLAY to the newly focused queue — its NEWEST entry takes the
    /// visible window immediately, replacing the previous queue's window,
    /// even when only one window fits.
    #[test]
    fn queue_switch_displays_newest_of_new_queue() {
        let mut state = RightPanelState::new();
        for agent in ["kilo", "opencode"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} v1\n"));
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} v2\n"));
        }
        // Sessions: 0=kilo v1 (superseded), 1=kilo v2, 2=open v1, 3=open v2.
        // Pin kilo at its oldest entry so its window occupies the display.
        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.set_queue_index("kilo", 0);

        let wrap_w = 48;
        // Budget fits exactly ONE window (a lone window carries no margin).
        let budget = i32::from(state.subagent_section_rows(wrap_w)[1]);
        state.resolve_visible_subagents(wrap_w, budget);
        assert_eq!(
            state.visible_subagents,
            vec![0],
            "precondition: kilo's pinned entry owns the single window"
        );

        // Switch to opencode → its newest entry takes over the SAME slot.
        assert!(state.cycle_agent_queue(1));
        assert_eq!(
            state.panel_focus,
            Some(PanelFocus::Agent("opencode".to_string()))
        );
        state.resolve_visible_subagents(wrap_w, budget);
        assert_eq!(
            state.visible_subagents,
            vec![3],
            "the new queue's newest session must replace the old window"
        );

        // The abandoned queue keeps its own place — a queue switch never
        // disturbs OTHER queues' navigation state.
        assert_eq!(state.queue_nav_index("kilo"), Some(0));
    }

    /// REGRESSION: arrow navigation is 100% confined to the focused queue —
    /// stepping past either end clamps in place and NEVER touches another
    /// CLI's nav state or entries.
    #[test]
    fn arrows_never_leak_into_other_queues() {
        let mut state = RightPanelState::new();
        for agent in ["kilo", "opencode"] {
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} v1\n"));
            state.start_pty(format!("subagent: {agent}"), None);
            state.complete_last_pty(format!("{agent} v2\n"));
        }
        // Focus opencode via a queue switch (its newest takes the display).
        assert!(state.cycle_agent_queue(1));

        // Hammer ← past the oldest entry: clamps at index 0…
        for _ in 0..5 {
            state.panel_left();
        }
        assert_eq!(state.queue_nav_index("opencode"), Some(0));
        // …and never pins or moves kilo's navigation.
        assert!(state.queue_nav_index("kilo").is_none());

        // Hammer → back to live and beyond: stays live.
        for _ in 0..5 {
            state.panel_right();
        }
        assert_eq!(state.queue_nav_index("opencode"), None);
        assert!(state.queue_nav_index("kilo").is_none());
    }

    /// REGRESSION: cycling with a SINGLE queue is a no-op — re-selecting
    /// the already-focused queue must not drop its navigation pin.
    #[test]
    fn cycling_sole_queue_keeps_its_pin() {
        let mut state = RightPanelState::new();
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("kilo v1\n".to_string());
        state.start_pty("subagent: kilo".to_string(), None);
        state.complete_last_pty("kilo v2\n".to_string());

        state.panel_focus = Some(PanelFocus::Agent("kilo".to_string()));
        state.set_queue_index("kilo", 0);

        assert!(state.cycle_agent_queue(1));
        assert!(state.cycle_agent_queue(-1));
        assert_eq!(
            state.queue_nav_index("kilo"),
            Some(0),
            "cycling the only queue must preserve its pin"
        );
        assert_eq!(
            state.panel_focus,
            Some(PanelFocus::Agent("kilo".to_string()))
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
        // The fence's painted layout keeps its inner content rows but NOT
        // a phantom trailing blank: estimate == painted by contract.
        let painted_estimate = estimate_height(&sanitize_subagent_text(body), w);
        assert_eq!(rows[0], 1 + 1 + painted_estimate);
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

        // Single-row selection keeps both x bounds on that row; the focus
        // column is INCLUSIVE (mirrors the highlight painter, `lx1..=lx2`),
        // so the glyph under the focus cell is copied too.
        state.begin_selection(1, 3);
        state.update_drag_selection(3, 3);
        assert_eq!(state.extract_selected_text(), "rav");

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
        // The focus column is inclusive (mirrors the highlight painter), so
        // the end row contributes the glyph under its focus column.
        let text = state.extract_selected_text();
        assert_eq!(text, "line0\nline1\nline2\nline3\nl");
    }
}
