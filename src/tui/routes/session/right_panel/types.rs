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
    /// Vertical scroll offset for the panel content.
    pub scroll_y: i32,
    /// Total content height in rows (computed during render).
    pub content_height: i32,
    /// Visible viewport height in rows (set during render).
    pub visible_height: i32,
}

impl RightPanelState {
    pub fn new() -> Self {
        Self {
            todos: Vec::new(),
            pty_sessions: Vec::new(),
            pending_todo_update_count: 0,
            next_pty_id: 0,
            scroll_y: 0,
            content_height: 0,
            visible_height: 0,
        }
    }

    /// Scroll up by `delta` lines.
    pub fn scroll_up(&mut self, delta: i32) {
        self.scroll_y = (self.scroll_y - delta).max(0);
    }

    /// Scroll down by `delta` lines.
    pub fn scroll_down(&mut self, delta: i32) {
        let max_scroll = (self.content_height - self.visible_height).max(0);
        self.scroll_y = (self.scroll_y + delta).min(max_scroll);
    }

    /// Scroll to the bottom of the content.
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = (self.content_height - self.visible_height).max(0);
        self.scroll_y = max_scroll;
    }

    /// Reset scroll to top.
    pub fn reset_scroll(&mut self) {
        self.scroll_y = 0;
    }

    /// Set the viewport height (called from render).
    pub fn set_visible_height(&mut self, h: i32) {
        self.visible_height = h;
        let max_scroll = (self.content_height - h).max(0);
        self.scroll_y = self.scroll_y.min(max_scroll);
    }

    /// Whether the user has scrolled up from the bottom (manual scroll).
    pub fn is_scrolled_up(&self) -> bool {
        let max_scroll = (self.content_height - self.visible_height).max(0);
        self.scroll_y < max_scroll
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
