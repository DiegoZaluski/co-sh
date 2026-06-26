#[derive(Debug, Clone, Copy, PartialEq)]
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
    pub depends_on: Vec<String>,
}

/// A named group of tasks.
#[derive(Debug, Clone)]
pub struct TaskGroup {
    pub title: String,
    pub items: Vec<TodoItem>,
    /// Whether the model has confirmed that tests were run for this group.
    /// Set to `false` on creation; set to `true` via `VerifyGroup` action.
    pub tests_verified: bool,
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

/// Output produced by mutation operations (`todo_write`, `todo_edit`, `todo_cross_off`).
#[derive(Debug, Clone)]
pub struct TodoWriteOutput {
    pub list: TodoList,
    pub nags: Vec<Nag>,
}

/// Output produced by `todo_read`.
#[derive(Debug, Clone)]
pub struct TodoReadOutput {
    pub groups: Vec<TaskGroup>,
    pub nags: Vec<Nag>,
}
