use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for `plan_todo_write`: the full desired TODO list, replacing the
/// previous one.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoWriteInput {
    pub todos: Vec<TodoItemInput>,
}

/// One task as submitted by the model in a `plan_todo_write` call.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoItemInput {
    /// Optional alias other tasks in this same call reference in
    /// `depends_on`; resolved to the assigned `task-N` id.
    pub key: Option<String>,
    pub description: String,
    /// Defaults to `pending` when omitted.
    #[serde(default = "default_status")]
    pub status: TodoStatus,
    /// Dependencies: sibling `key`s or `task-N` ids.
    pub depends_on: Option<Vec<String>>,
}

fn default_status() -> TodoStatus {
    TodoStatus::Pending
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        })
    }
}

/// One task in the flat TODO list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub description: String,
    pub status: TodoStatus,
    pub depends_on: Vec<String>,
}

/// The full mutable state of the todo system: a flat list of tasks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Nag {
    pub message: String,
}

/// Output produced by `todo_write`.
#[derive(Debug, Clone, Serialize)]
pub struct TodoWriteOutput {
    pub list: TodoList,
    pub nags: Vec<Nag>,
}

/// Error type for plan operations.
/// Wraps human-readable error messages intended as correction prompts for the model.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);
