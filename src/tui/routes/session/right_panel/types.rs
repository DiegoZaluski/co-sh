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

    // ── Per-section scroll state ───────────────────────────────────
    /// Scroll offset within the TODO section (only when items overflow).
    pub todo_scroll_y: i32,
    /// Scroll offset within the bash section (shared across all bash PTYs).
    pub bash_scroll_y: i32,
    /// Scroll offset within the subagent section (shared across all subagent PTYs).
    pub subagent_scroll_y: i32,

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
            todo_scroll_y: 0,
            bash_scroll_y: 0,
            subagent_scroll_y: 0,
            user_scrolled_away: false,
            scroll_y: 0,
            content_height: 0,
            visible_height: 0,
        }
    }

    /// Scroll the first section with overflow (prefers bash > subagent > todo) up by `delta` lines.
    pub fn scroll_up(&mut self, delta: i32) {
        self.user_scrolled_away = true;
        if self
            .pty_sessions
            .iter()
            .any(|p| !p.command.starts_with("subagent:"))
        {
            self.bash_scroll_y = (self.bash_scroll_y - delta).max(0);
        } else if self.pty_sessions.iter().any(|p| p.command.starts_with("subagent:")) {
            self.subagent_scroll_y = (self.subagent_scroll_y - delta).max(0);
        } else if !self.todos.is_empty() {
            self.todo_scroll_y = (self.todo_scroll_y - delta).max(0);
        }
    }

    /// Scroll the first section with overflow (prefers bash > subagent > todo) down by `delta` lines.
    pub fn scroll_down(&mut self, delta: i32) {
        self.user_scrolled_away = true;
        if self
            .pty_sessions
            .iter()
            .any(|p| !p.command.starts_with("subagent:"))
        {
            self.bash_scroll_y = (self.bash_scroll_y + delta).max(0);
        } else if self.pty_sessions.iter().any(|p| p.command.starts_with("subagent:")) {
            self.subagent_scroll_y = (self.subagent_scroll_y + delta).max(0);
        } else if !self.todos.is_empty() {
            self.todo_scroll_y = (self.todo_scroll_y + delta).max(0);
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
    }

    /// Start a new PTY session for a bash command.
    pub fn start_pty(&mut self, command: String, workdir: Option<String>) {
        self.next_pty_id += 1;
        let id = format!("pty-{}", self.next_pty_id);
        self.pty_sessions.push(PtySession {
            id,
            command,
            output: String::new(),
            workdir,
            status: PtyStatus::Running,
        });
    }

    /// Append output for the last running PTY session.
    pub fn update_last_pty(&mut self, output: String) {
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            session.output.push_str(&output);
        }
    }

    /// Mark the last running PTY session as completed.
    pub fn complete_last_pty(&mut self, final_output: String) {
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            session.output = final_output;
            session.status = PtyStatus::Completed;
        }
    }

    /// Mark the last running PTY session as failed.
    pub fn fail_last_pty(&mut self, error: String) {
        if let Some(session) = self
            .pty_sessions
            .iter_mut()
            .rev()
            .find(|s| matches!(s.status, PtyStatus::Running))
        {
            session.output = error;
            session.status = PtyStatus::Failed;
        }
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
}
