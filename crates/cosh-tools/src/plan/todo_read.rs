//! Query operations for the todo list.
//!
//! Supports listing tasks (optionally filtered by status) and
//! fetching a single task by ID.

use super::types::{TodoItem, TodoList, TodoReadAction, TodoReadOutput, TodoStatus};

/// Query the todo list.
///
/// # Errors
///
/// Returns `Err` if a `Get` targets a non-existent task.
pub fn todo_read(list: &TodoList, action: &TodoReadAction) -> Result<TodoReadOutput, String> {
    match action {
        TodoReadAction::List { status } => Ok(list_tasks(list, status.as_ref())),
        TodoReadAction::Get { id } => get_task(list, id),
    }
}

fn list_tasks(list: &TodoList, status: Option<&TodoStatus>) -> TodoReadOutput {
    let items: Vec<TodoItem> = match status {
        Some(s) => list
            .items
            .iter()
            .filter(|i| i.status == *s)
            .cloned()
            .collect(),
        None => list.items.clone(),
    };

    TodoReadOutput {
        items,
        nags: Vec::new(),
    }
}

fn get_task(list: &TodoList, id: &str) -> Result<TodoReadOutput, String> {
    let item = list
        .items
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found."))?;

    Ok(TodoReadOutput {
        items: vec![item.clone()],
        nags: Vec::new(),
    })
}
