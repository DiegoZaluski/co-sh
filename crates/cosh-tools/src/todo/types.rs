use std::time::SystemTime;

/// Status of a todo item in the state machine.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// Simple boolean tags for a todo item.
///
/// Tags are deliberately kept simple — no complex taxonomies.
/// `possibly_important` signals that the rationale behind this task
/// may be worth preserving after completion.
#[derive(Debug, Clone, Default)]
pub struct TodoTags {
    /// If `true`, completing this task will require a rationale
    /// and may produce a [`PendingResolution`] after human confirmation.
    pub possibly_important: bool,
}

/// A single tracked item.
#[derive(Debug, Clone)]
pub struct TodoItem {
    /// Opaque, caller-assigned identifier (e.g. a UUID or short slug).
    pub id: String,
    /// Human-readable description of the task.
    pub description: String,
    pub status: TodoStatus,
    pub tags: TodoTags,
    /// Expected wall-clock duration in milliseconds.
    /// When set, the tool warns if the task is still open past this window.
    pub timeline_ms: Option<u64>,
    /// IDs of other items that must reach `Completed` before this one can start.
    pub depends_on: Vec<String>,
    /// Rationale captured at completion time (required when `possibly_important`).
    pub rationale: Option<String>,
    /// Category tags for the resolution (e.g. "bugfix", "architectural").
    pub resolution_tags: Vec<String>,
    /// Unix timestamp (ms) of creation.
    pub created_at: u64,
    /// Unix timestamp (ms) of last status / metadata change.
    pub updated_at: u64,
}

/// The full mutable state of the todo system.
///
/// Ownership lives with the caller — the tool never persists anything
/// internally. Pass the current list in, get an updated list back.
#[derive(Debug, Clone, Default)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

/// Action to perform against the todo list.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TodoAction {
    /// Append a new task.
    Add {
        description: String,
        tags: Option<TodoTags>,
        timeline_ms: Option<u64>,
        depends_on: Option<Vec<String>>,
    },
    /// Move a task to `InProgress`.
    Start { id: String },
    /// Mark a task as completed.
    ///
    /// If the task was tagged `possibly_important`, a non-empty
    /// `rationale` is required — the tool will return `Err` without one.
    Complete {
        id: String,
        rationale: Option<String>,
        resolution_tags: Option<Vec<String>>,
    },
    /// Mark a task as cancelled.
    Cancel { id: String },
    /// Change metadata of an existing task.
    Update {
        id: String,
        description: Option<String>,
        tags: Option<TodoTags>,
        timeline_ms: Option<u64>,
        depends_on: Option<Vec<String>>,
    },
    /// Remove a task entirely from the list.
    Remove { id: String },
    /// Confirm that a completed important task was successful.
    ///
    /// This is the only path that produces a [`PendingResolution`].
    /// The caller is expected to have obtained **explicit human
    /// confirmation** before issuing this action.
    ConfirmResolution {
        todo_id: String,
        rationale: String,
        resolution_tags: Vec<String>,
    },
    /// Discard a pending resolution — the task is kept as completed
    /// but no resolution record is produced.
    DiscardResolution { todo_id: String },
    /// Sweep completed / cancelled items from the list.
    Clean { keep_pending: bool },
}

/// A resolution record produced when a completed important task
/// receives human confirmation.
///
/// The tool returns these in [`TodoWriteOutput::pending_resolutions`].
/// The caller decides where — and whether — to persist them.
#[derive(Debug, Clone)]
pub struct PendingResolution {
    pub todo_id: String,
    pub description: String,
    pub rationale: String,
    pub tags: Vec<String>,
}

/// Severity of a nag message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NagSeverity {
    /// A non-blocking reminder (the action still succeeds).
    Warning,
    /// A blocking validation failure (the action is rejected).
    Error,
}

/// A reminder or validation message directed at the LLM.
#[derive(Debug, Clone)]
pub struct Nag {
    pub severity: NagSeverity,
    pub message: String,
}

/// Output produced by [`todowrite`](super::todowrite::todowrite).
#[derive(Debug, Clone)]
pub struct TodoWriteOutput {
    /// The updated todo list after the action was applied.
    pub list: TodoList,
    /// Warnings and reminders for the LLM.
    pub nags: Vec<Nag>,
    /// Resolutions that are waiting for (or have received) human confirmation.
    pub pending_resolutions: Vec<PendingResolution>,
}

// Helpers

pub(crate) fn now_ms() -> u64 {
    // move to util after
    u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
