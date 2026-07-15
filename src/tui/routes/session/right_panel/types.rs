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
}

impl RightPanelState {
    pub fn new() -> Self {
        Self {
            todos: Vec::new(),
            pty_sessions: Vec::new(),
            pending_todo_update_count: 0,
            next_pty_id: 0,
        }
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

    /// Update output for the last running PTY session.
    pub fn update_last_pty(&mut self, output: String) {
        if let Some(session) = self.pty_sessions.iter_mut().rev().find(|s| matches!(s.status, PtyStatus::Running)) {
            session.output = output;
        }
    }

    /// Mark the last running PTY session as completed.
    pub fn complete_last_pty(&mut self, final_output: String) {
        if let Some(session) = self.pty_sessions.iter_mut().rev().find(|s| matches!(s.status, PtyStatus::Running)) {
            session.output = final_output;
            session.status = PtyStatus::Completed;
        }
    }

    /// Mark the last running PTY session as failed.
    pub fn fail_last_pty(&mut self, error: String) {
        if let Some(session) = self.pty_sessions.iter_mut().rev().find(|s| matches!(s.status, PtyStatus::Running)) {
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
