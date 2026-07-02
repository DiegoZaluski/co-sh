use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::todo_cross_off::TodoCrossOff;
use super::todo_edit::TodoEdit;
use super::todo_write::TodoWriteAction;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// Input for `plan_todo_read`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoReadInput {
    pub action: TodoReadAction,
}

/// Input for `plan_todo_write`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoWriteInput {
    pub action: TodoWriteAction,
}

/// Input for `plan_todo_edit`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoEditInput {
    pub edit: TodoEdit,
}

/// Input for `plan_todo_cross_off`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoCrossOffInput {
    pub action: TodoCrossOff,
}

/// Input for `plan_load_from_md`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TodoLoadFromMdInput {
    pub path: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TodoReadAction {
    List {
        group: Option<String>,
        status: Option<TodoStatus>,
    },
    Get {
        id: String,
    },
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

/// Error type for plan operations.
/// Wraps human-readable error messages intended as correction prompts for the model.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);
