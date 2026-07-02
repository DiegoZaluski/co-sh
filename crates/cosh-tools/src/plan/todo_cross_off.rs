use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::types::{Nag, PlanError, TodoList, TodoStatus, TodoWriteOutput};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TodoCrossOff {
    Complete { id: String },
    Cancel { id: String },
}

/// Mark a task as Complete or Cancelled.
///
/// # Errors
///
/// Returns `Err` if the task does not exist or is already in a terminal state.
pub fn todo_cross_off(
    list: &TodoList,
    action: &TodoCrossOff,
) -> Result<TodoWriteOutput, PlanError> {
    match action {
        TodoCrossOff::Complete { id } => set_terminal(list, id, TodoStatus::Completed),
        TodoCrossOff::Cancel { id } => set_terminal(list, id, TodoStatus::Cancelled),
    }
}

fn set_terminal(
    list: &TodoList,
    id: &str,
    status: TodoStatus,
) -> Result<TodoWriteOutput, PlanError> {
    let mut groups = list.groups.clone();
    let (gi, ii) = super::todo_write::find_item(&groups, id).ok_or_else(|| {
        PlanError(format!(
            "Task '{id}' does not exist. Use List to see available tasks."
        ))
    })?;

    let item = &groups[gi].items[ii];
    match item.status {
        TodoStatus::Completed => {
            return Err(PlanError(format!("Task '{id}' is already completed.")));
        }
        TodoStatus::Cancelled => {
            return Err(PlanError(format!("Task '{id}' is already cancelled.")));
        }
        TodoStatus::InProgress | TodoStatus::Pending => {}
    }

    groups[gi].items[ii].status = status;

    let mut nags = Vec::new();

    // Nag about any items that depend on this one.
    for g in &groups {
        for i in &g.items {
            let blocked = i.status != TodoStatus::Completed && i.status != TodoStatus::Cancelled;
            if i.depends_on.iter().any(|d| d == id) && blocked {
                nags.push(Nag {
                    message: format!("'{}' depends on '{id}' which is now {:?}.", i.id, status),
                });
            }
        }
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}
