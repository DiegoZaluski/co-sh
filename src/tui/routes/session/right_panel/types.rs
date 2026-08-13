/// Maximum number of PTY sessions (bash + subagent) kept in the right panel.
/// Older sessions are evicted first so the panel cannot grow without bound
/// over the process lifetime (restarting the app used to be the only way to
/// clear the accumulated output).
const MAX_PTY_SESSIONS: usize = 20;

/// Maximum number of characters of output retained per PTY session.
/// Only the TAIL is kept, since the panel renders the most recent output.
const MAX_PTY_OUTPUT_CHARS: usize = 60_000;

use cosh_tui::core::renderables::markdown::estimate_height;
use ratatui::buffer::Buffer;

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
    /// Per-section activation order values. Index by `section_kind_index()`.
    section_activity_order: [u64; 3],

    // ── Derived buffer caches (avoid rebuilding all accumulated output per frame) ──
    /// Bumped on every PTY mutation; the derived text buffers are rebuilt only
    /// when this changes. Without this, every frame rebuilt the FULL output of
    /// every bash/subagent session twice (height calc + render).
    pty_gen: u64,
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
    /// `pty_gen` and the wrap width. Rebuilt only when output or width
    /// changes — `estimate_height` (a pulldown_cmark parse) is too
    /// expensive to run on every frame.
    subagent_rows_cache: Vec<u16>,
    subagent_rows_cache_gen: u64,
    subagent_rows_cache_w: u16,
    /// Reusable scratch buffer for blitting the visible slice of a session's
    /// markdown body: the markdown renderer lays out from content row 0, so
    /// the full body is rendered here and only the visible rows are copied
    /// into the panel. Kept across frames — `Buffer::resize` retains the
    /// allocation, so streaming never allocates a large buffer per frame.
    pub(crate) subagent_scratch: Option<Buffer>,

    // ── Auto-scroll tracking ────────────────────────────────────────
    /// Set to `true` when the user manually scrolls (up/down);
    /// set to `false` by `scroll_to_bottom()`. Used by `is_scrolled_up()`
    /// to decide whether to auto-scroll on new output.
    pub user_scrolled_away: bool,

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
            bash_buffer_cache: Vec::new(),
            bash_cache_gen: 0,
            subagent_buffer_cache: Vec::new(),
            subagent_cache_gen: 0,
            todo_scroll_y: 0,
            bash_scroll_y: 0,
            subagent_scroll_y: 0,
            subagent_rows_cache: Vec::new(),
            subagent_rows_cache_gen: 0,
            subagent_rows_cache_w: 0,
            subagent_scratch: None,
            user_scrolled_away: false,
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

    /// Scroll the first section with overflow (prefers bash > subagent > todo) up by `delta` lines.
    ///
    /// Uses saturating arithmetic: the stored offsets may hold the `i32::MAX`
    /// sentinel set by [`scroll_to_bottom`](Self::scroll_to_bottom) until the
    /// render path clamps them (which only happens when content overflows).
    /// A raw `+ delta` would overflow and panic in debug builds.
    pub fn scroll_up(&mut self, delta: i32) {
        self.user_scrolled_away = true;
        if self
            .pty_sessions
            .iter()
            .any(|p| !p.command.starts_with("subagent:"))
        {
            self.bash_scroll_y = self.bash_scroll_y.saturating_sub(delta).max(0);
        } else if self
            .pty_sessions
            .iter()
            .any(|p| p.command.starts_with("subagent:"))
        {
            self.subagent_scroll_y = self.subagent_scroll_y.saturating_sub(delta).max(0);
        } else if !self.todos.is_empty() {
            self.todo_scroll_y = self.todo_scroll_y.saturating_sub(delta).max(0);
        }
    }

    /// Scroll the first section with overflow (prefers bash > subagent > todo) down by `delta` lines.
    ///
    /// Uses saturating arithmetic: the stored offsets may hold the `i32::MAX`
    /// sentinel set by [`scroll_to_bottom`](Self::scroll_to_bottom) until the
    /// render path clamps them (which only happens when content overflows).
    /// A raw `+ delta` would overflow and panic in debug builds.
    pub fn scroll_down(&mut self, delta: i32) {
        self.user_scrolled_away = true;
        if self
            .pty_sessions
            .iter()
            .any(|p| !p.command.starts_with("subagent:"))
        {
            self.bash_scroll_y = self.bash_scroll_y.saturating_add(delta).max(0);
        } else if self
            .pty_sessions
            .iter()
            .any(|p| p.command.starts_with("subagent:"))
        {
            self.subagent_scroll_y = self.subagent_scroll_y.saturating_add(delta).max(0);
        } else if !self.todos.is_empty() {
            self.todo_scroll_y = self.todo_scroll_y.saturating_add(delta).max(0);
        }
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
    /// body) per subagent session, cached against `pty_gen` and `wrap_w` so
    /// the markdown parse only runs when the output or the wrap width
    /// actually changed. Mirrors the render exactly: the body width used
    /// here is the same `wrap_w` the section renderer passes to
    /// [`MarkdownRenderable`](cosh_tui::core::renderables::markdown::MarkdownRenderable).
    pub(crate) fn subagent_section_rows(&mut self, wrap_w: u16) -> &[u16] {
        if self.pty_gen != self.subagent_rows_cache_gen || wrap_w != self.subagent_rows_cache_w {
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
            self.subagent_rows_cache_gen = self.pty_gen;
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
        state.scroll_to_bottom();

        // Reproduce the post-resize scroll: offset is still i32::MAX because
        // the content fits the viewport, so no render clamp has run.
        // Scrolling down while pinned to the bottom must be a no-op
        // (saturating), never an overflow panic.
        state.scroll_down(3);
        assert_eq!(state.bash_scroll_y, i32::MAX);

        state.scroll_down(20);
        assert_eq!(state.bash_scroll_y, i32::MAX);

        // And scrolling up from the bottom sentinel must work normally.
        state.scroll_up(5);
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
        sub.scroll_to_bottom();
        sub.scroll_down(3);
        assert_eq!(sub.subagent_scroll_y, i32::MAX);
        sub.scroll_up(5);
        assert_eq!(sub.subagent_scroll_y, i32::MAX - 5);

        // Todo branch: only todos present, no PTYs.
        let mut todos = RightPanelState::new();
        todos.set_todos(vec![TodoItem {
            status: "pending".to_string(),
            content: "do the thing".to_string(),
        }]);
        todos.scroll_to_bottom();
        todos.scroll_down(3);
        assert_eq!(todos.todo_scroll_y, i32::MAX);
        todos.scroll_up(5);
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
}
