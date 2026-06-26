use std::fmt::Write;

use super::types::{Nag, TodoItem, TodoList, TodoStatus, TodoWriteOutput};

pub enum TodoWriteAction {
    Add {
        group: String,
        description: String,
        depends_on: Option<Vec<String>>,
    },
    Start { id: String },
    Remove { id: String },
    Clean { keep_pending: bool },
}

/// Apply a mutation action to the todo list.
///
/// # Errors
///
/// Returns `Err` with an explanation when:
/// - An `Add` action has an empty description.
/// - A `Start` targets a non-existent or already-in-progress item.
/// - Dependencies are not satisfied.
pub fn todo_write(list: &TodoList, action: &TodoWriteAction) -> Result<TodoWriteOutput, String> {
    match action {
        TodoWriteAction::Add {
            group,
            description,
            depends_on,
        } => add(list, group, description, depends_on.as_ref()),
        TodoWriteAction::Start { id } => start(list, id),
        TodoWriteAction::Remove { id } => remove(list, id),
        TodoWriteAction::Clean { keep_pending } => Ok(clean(list, *keep_pending)),
    }
}

fn add(
    list: &TodoList,
    group: &str,
    description: &str,
    depends_on: Option<&Vec<String>>,
) -> Result<TodoWriteOutput, String> {
    if description.trim().is_empty() {
        return Err("Description must be non-empty text.".into());
    }

    let mut nags: Vec<Nag> = Vec::new();
    let mut groups = list.groups.clone();
    let id = next_id(&groups);

    if let Some(deps) = depends_on {
        for dep_id in deps {
            if *dep_id == id {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' is a self-reference."),
                });
            } else if !item_exists(&groups, dep_id) {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' does not exist in the task list."),
                });
            }
        }
    }

    let item = TodoItem {
        id: id.clone(),
        description: description.to_owned(),
        status: TodoStatus::Pending,
        depends_on: depends_on.cloned().unwrap_or_default(),
    };

    if let Some(g) = groups.iter_mut().find(|g| g.title == group) {
        g.items.push(item);
    } else {
        groups.push(super::types::TaskGroup {
            title: group.to_owned(),
            items: vec![item],
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}

fn start(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut groups = list.groups.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let (gi, ii) = find_item(&groups, id).ok_or_else(|| {
        format!("Task '{id}' does not exist. Use List to see available tasks.")
    })?;

    let others_in_progress: Vec<&str> = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| i.status == TodoStatus::InProgress && i.id != id)
        .map(|i| i.id.as_str())
        .collect();

    if !others_in_progress.is_empty() {
        return Err(format!(
            "Only one task at a time can be InProgress. Complete or cancel {} first before starting '{id}'.",
            join_ids(&others_in_progress),
        ));
    }

    let deps = groups[gi].items[ii].depends_on.clone();
    for dep_id in &deps {
        let dep = find_item_in_groups(&groups, dep_id);
        match dep {
            Some(d) if d.status != TodoStatus::Completed => {
                nags.push(Nag {
                    message: format!(
                        "'{id}' depends on '{dep_id}' which is still {:?}.",
                        d.status
                    ),
                });
            }
            None => {
                nags.push(Nag {
                    message: format!(
                        "'{id}' depends on '{dep_id}' which no longer exists."
                    ),
                });
            }
            _ => {}
        }
    }

    groups[gi].items[ii].status = TodoStatus::InProgress;

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}

fn remove(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut groups = list.groups.clone();
    let mut removed = false;

    for g in &mut groups {
        if let Some(pos) = g.items.iter().position(|i| i.id == id) {
            g.items.remove(pos);
            removed = true;
            break;
        }
    }

    if !removed {
        return Err(format!(
            "Task '{id}' does not exist. Use List to see available tasks."
        ));
    }

    let stale_refs: Vec<String> = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| i.depends_on.contains(&id.to_string()))
        .map(|i| i.id.clone())
        .collect();

    let mut nags = Vec::new();
    if !stale_refs.is_empty() {
        nags.push(Nag {
            message: format!(
                "'{id}' was removed but is still referenced as a dependency by {}.",
                join_ids(&stale_refs.iter().map(String::as_str).collect::<Vec<_>>()),
            ),
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}

fn clean(list: &TodoList, keep_pending: bool) -> TodoWriteOutput {
    let mut groups = list.groups.clone();
    let mut total_removed = 0usize;

    for g in &mut groups {
        let len_before = g.items.len();
        g.items.retain(|i| match i.status {
            TodoStatus::Pending if keep_pending => true,
            TodoStatus::InProgress => true,
            _ => false,
        });
        total_removed += len_before - g.items.len();
    }

    let mut nags: Vec<Nag> = Vec::new();
    if total_removed > 0 {
        nags.push(Nag {
            message: format!("Removed {total_removed} completed or cancelled tasks."),
        });
    }

    TodoWriteOutput {
        list: TodoList { groups },
        nags,
    }
}

// Internal helpers exported for sibling modules
pub(super) fn next_id(groups: &[super::types::TaskGroup]) -> String {
    let max = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter_map(|i| i.id.strip_prefix("task-"))
        .filter_map(|s| s.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    format!("task-{}", max + 1)
}

pub(super) fn find_item(groups: &[super::types::TaskGroup], id: &str) -> Option<(usize, usize)> {
    groups
        .iter()
        .enumerate()
        .find_map(|(gi, g)| g.items.iter().position(|i| i.id == id).map(|ii| (gi, ii)))
}

pub(super) fn item_exists(groups: &[super::types::TaskGroup], id: &str) -> bool {
    groups.iter().any(|g| g.items.iter().any(|i| i.id == id))
}

pub(super) fn find_item_in_groups<'a>(
    groups: &'a [super::types::TaskGroup],
    id: &str,
) -> Option<&'a TodoItem> {
    groups
        .iter()
        .flat_map(|g| &g.items)
        .find(|i| i.id == id)
}

fn join_ids(ids: &[&str]) -> String {
    match ids.len() {
        0 => String::new(),
        1 => format!("'{}'", ids[0]),
        2 => format!("'{}' and '{}'", ids[0], ids[1]),
        _ => {
            let mut s = ids[..ids.len() - 1]
                .iter()
                .map(|id| format!("'{id}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(s, ", and '{}'", ids[ids.len() - 1]);
            s
        }
    }
}
