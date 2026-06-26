use std::time::SystemTime;

/// Status of a todo item.
#[derive(Debug, Clone, PartialEq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// A single tracked task.
#[derive(Debug, Clone)]
pub struct TodoItem {
    /// Opaque identifier (e.g. `task-1`).
    pub id: String,
    /// Human-readable description.
    pub description: String,
    pub status: TodoStatus,
    /// Expected wall-clock duration in milliseconds. Warns when exceeded.
    pub timeline_ms: Option<u64>,
    /// IDs of items that must reach `Completed` before this one can start.
    pub depends_on: Vec<String>,
    /// Unix timestamp (ms) of creation.
    pub created_at: u64,
    /// Unix timestamp (ms) of last status or metadata change.
    pub updated_at: u64,
}

/// The full mutable state of the todo system.
///
/// Ownership lives with the caller. Pass the current list in, get an
/// updated list back. The tool never persists anything internally.
#[derive(Debug, Clone, Default)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

/// A mutation action against the todo list.
#[derive(Debug, Clone)]
pub enum TodoAction {
    /// Append a new task.
    Add {
        description: String,
        /// Optional wall-clock budget in milliseconds.
        timeline_ms: Option<u64>,
        /// IDs this task depends on.
        depends_on: Option<Vec<String>>,
    },
    /// Move a task to `InProgress`.
    Start { id: String },
    /// Mark a task as completed.
    Complete { id: String },
    /// Mark a task as cancelled.
    Cancel { id: String },
    /// Change metadata of an existing task.
    Update {
        id: String,
        description: Option<String>,
        timeline_ms: Option<u64>,
        depends_on: Option<Vec<String>>,
    },
    /// Remove a task entirely from the list.
    Remove { id: String },
    /// Sweep completed / cancelled items from the list.
    Clean { keep_pending: bool },
}

/// A read / query action against the todo list.
#[derive(Debug, Clone)]
pub enum TodoReadAction {
    /// List all tasks, optionally filtered by status.
    List { status: Option<TodoStatus> },
    /// Return a single task by ID.
    Get { id: String },
}

/// A non-blocking advisory message returned alongside a result.
///
/// Nags inform the caller about potential issues (stale dependencies,
/// exceeded timelines, etc.) without blocking the action.
#[derive(Debug, Clone)]
pub struct Nag {
    pub message: String,
}

/// Output produced by [`todo_write`](super::todowrite::todo_write).
#[derive(Debug, Clone)]
pub struct TodoWriteOutput {
    /// The updated todo list after the action was applied.
    pub list: TodoList,
    /// Warnings and reminders.
    pub nags: Vec<Nag>,
}

/// Output produced by [`todo_read`](super::todoread::todo_read).
#[derive(Debug, Clone)]
pub struct TodoReadOutput {
    /// The matching items (one for `Get`, zero or more for `List`).
    pub items: Vec<TodoItem>,
    /// Warnings and reminders.
    pub nags: Vec<Nag>,
}

pub(crate) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
