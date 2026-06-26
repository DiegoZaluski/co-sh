use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub id: String,
    pub description: String,
    pub status: TodoStatus,
    pub timeline_ms: Option<u64>,
    pub depends_on: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A named group of tasks.
#[derive(Debug, Clone)]
pub struct TaskGroup {
    pub title: String,
    pub items: Vec<TodoItem>,
}

/// The full mutable state of the todo system.
#[derive(Debug, Clone, Default)]
pub struct TodoList {
    pub groups: Vec<TaskGroup>,
}

#[derive(Debug, Clone)]
pub enum TodoReadAction {
    List {
        group: Option<String>,
        status: Option<TodoStatus>,
    },
    Get { id: String },
}

#[derive(Debug, Clone)]
pub struct Nag {
    pub message: String,
}

/// Output produced by mutation operations (todo_write, todo_edit, todo_cross_off).
#[derive(Debug, Clone)]
pub struct TodoWriteOutput {
    pub list: TodoList,
    pub nags: Vec<Nag>,
}

/// Output produced by [`todo_read`](super::todo_read::todo_read).
#[derive(Debug, Clone)]
pub struct TodoReadOutput {
    pub groups: Vec<TaskGroup>,
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
