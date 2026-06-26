use super::types::{Nag, TodoList, TodoStatus, TodoWriteOutput};

pub enum TodoCrossOff {
    Complete { id: String },
    Cancel { id: String },
}

/// Mark a task as Complete or Cancelled.
///
/// # Errors
///
/// Returns `Err` if the task does not exist or is already in a terminal state.
pub fn todo_cross_off(list: &TodoList, action: &TodoCrossOff) -> Result<TodoWriteOutput, String> {
    match action {
        TodoCrossOff::Complete { id } => set_terminal(list, id, TodoStatus::Completed),
        TodoCrossOff::Cancel { id } => set_terminal(list, id, TodoStatus::Cancelled),
    }
}

fn set_terminal(
    list: &TodoList,
    id: &str,
    status: TodoStatus,
) -> Result<TodoWriteOutput, String> {
    let mut groups = list.groups.clone();
    let (gi, ii) = super::todo_write::find_item(&groups, id).ok_or_else(|| {
        format!("Task '{id}' does not exist. Use List to see available tasks.")
    })?;

    let item = &groups[gi].items[ii];
    match item.status {
        TodoStatus::Completed => return Err(format!("Task '{id}' is already completed.")),
        TodoStatus::Cancelled => return Err(format!("Task '{id}' is already cancelled.")),
        TodoStatus::InProgress | TodoStatus::Pending => {}
    }

    groups[gi].items[ii].status = status.clone();

    let mut nags = Vec::new();

    // Nag about any items that depend on this one.
    for g in &groups {
        for i in &g.items {
            if i.depends_on.iter().any(|d| d == id) && i.status == TodoStatus::Pending {
                nags.push(Nag {
                    message: format!(
                        "'{}' depends on '{id}' which is now {:?}.",
                        i.id, status
                    ),
                });
            }
        }
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}
