use super::types::{Nag, TaskGroup, TodoList, TodoReadAction, TodoReadOutput, TodoStatus};

/// Query the todo list.
///
/// # Errors
///
/// Returns `Err` if a `Get` targets a non-existent task.
pub fn todo_read(list: &TodoList, action: &TodoReadAction) -> Result<TodoReadOutput, String> {
    match action {
        TodoReadAction::List { group, status } => Ok(list_tasks(list, group.as_deref(), status.as_ref())),
        TodoReadAction::Get { id } => get_task(list, id),
    }
}

fn list_tasks(
    list: &TodoList,
    group: Option<&str>,
    status: Option<&TodoStatus>,
) -> TodoReadOutput {
    let groups: Vec<TaskGroup> = list
        .groups
        .iter()
        .filter(|g| group.is_none_or(|gname| g.title == gname))
        .map(|g| TaskGroup {
            title: g.title.clone(),
            items: g
                .items
                .iter()
                .filter(|i| status.is_none_or(|s| i.status == *s))
                .cloned()
                .collect(),
            tests_verified: g.tests_verified,
        })
        .collect();

    let nags = verification_nags(list);

    TodoReadOutput { groups, nags }
}

/// Generate nags for groups where all tasks are terminal (completed/cancelled)
/// but the model has not confirmed tests via `VerifyGroup`.
fn verification_nags(list: &TodoList) -> Vec<Nag> {
    list.groups
        .iter()
        .filter(|g| {
            !g.tests_verified
                && !g.items.is_empty()
                && g.items
                    .iter()
                    .all(|i| matches!(i.status, TodoStatus::Completed | TodoStatus::Cancelled))
        })
        .map(|g| Nag {
            message: format!(
                "Group '{}' is complete but tests have not been confirmed. \
                 Use VerifyGroup to confirm tests passed.",
                g.title
            ),
        })
        .collect()
}

fn get_task(list: &TodoList, id: &str) -> Result<TodoReadOutput, String> {
    for g in &list.groups {
        if let Some(item) = g.items.iter().find(|i| i.id == id) {
            return Ok(TodoReadOutput {
                groups: vec![TaskGroup {
                    title: g.title.clone(),
                    items: vec![item.clone()],
                    tests_verified: g.tests_verified,
                }],
                nags: Vec::new(),
            });
        }
    }

    Err(format!("Task '{id}' not found."))
}
