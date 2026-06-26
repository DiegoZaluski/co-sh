//! Mutation operations for the todo list.
//!
//! Each [`TodoAction`] variant is dispatched to a pure function that
//! validates invariants, mutates the list, and returns a
//! [`TodoWriteOutput`] with the new state and diagnostics (nags).
//!
//! # State machine
//!
//! ```text
//! ┌─────────┐   Start    ┌────────────┐   Complete   ┌───────────┐
//! │ Pending │ ─────────→ │ InProgress │ ───────────→ │ Completed │
//! └─────────┘            └────────────┘              └───────────┘
//!      ↓                                                ↓
//!  Cancel                                            Cancel
//!      ↓                                                ↓
//! ┌───────────┐                                    ┌───────────┐
//! │ Cancelled │                                    │ Cancelled │
//! └───────────┘                                    └───────────┘
//! ```
//!
//! # Actions
//!
//! | Action | Effect |
//! |---|---|
//! | [`Add`](TodoAction::Add) | Append a new task with optional timeline and dependencies |
//! | [`Start`](TodoAction::Start) | Transition a task to `InProgress`; rejects if another is already in progress |
//! | [`Complete`](TodoAction::Complete) | Mark done |
//! | [`Cancel`](TodoAction::Cancel) | Mark cancelled |
//! | [`Update`](TodoAction::Update) | Modify description, timeline, or dependencies |
//! | [`Remove`](TodoAction::Remove) | Delete a task and warn about stale dependency refs |
//! | [`Clean`](TodoAction::Clean) | Sweep completed/cancelled tasks (optionally keep pending) |
//!
//! # Nag system
//!
//! Non-blocking diagnostics (warnings) are returned as [`Nag`] items
//! alongside every successful result. A nag never blocks an action; it is
//! advisory only.
//!
//! # Errors
//!
//! The [`todo_write`] function returns `Err(String)` with a
//! human-readable explanation when invariants are violated:
//!
//! - Empty description on `Add` or `Update`
//! - Starting a non-existent task
//! - Two tasks `InProgress` concurrently
//! - Referencing a non-existent dependency

use std::fmt::Write;

use super::types::{Nag, TodoAction, TodoItem, TodoList, TodoStatus, TodoWriteOutput, now_ms};

/// Apply a mutation action to the todo list.
///
/// # Errors
///
/// Returns `Err` with an explanation when:
/// - An `Add` action has an empty description.
/// - A `Start` targets a non-existent or already-in-progress item.
/// - Dependencies are not satisfied.
pub fn todo_write(list: &TodoList, action: &TodoAction) -> Result<TodoWriteOutput, String> {
    match action {
        TodoAction::Add {
            description,
            timeline_ms,
            depends_on,
        } => add(list, description, *timeline_ms, depends_on.as_ref()),
        TodoAction::Start { id } => start(list, id),
        TodoAction::Complete { id } => complete(list, id),
        TodoAction::Cancel { id } => cancel(list, id),
        TodoAction::Update {
            id,
            description,
            timeline_ms,
            depends_on,
        } => update(
            list,
            id,
            description.as_ref(),
            *timeline_ms,
            depends_on.as_ref(),
        ),
        TodoAction::Remove { id } => remove(list, id),
        TodoAction::Clean { keep_pending } => Ok(clean(list, *keep_pending)),
    }
}

fn add(
    list: &TodoList,
    description: &str,
    timeline_ms: Option<u64>,
    depends_on: Option<&Vec<String>>,
) -> Result<TodoWriteOutput, String> {
    if description.trim().is_empty() {
        return Err("Cannot add a task with an empty description.".into());
    }

    let mut nags: Vec<Nag> = Vec::new();
    let mut items = list.items.clone();
    let id = next_id(&items);

    if let Some(deps) = depends_on {
        for dep_id in deps {
            if *dep_id == id {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' is a self-reference."),
                });
            } else if !items.iter().any(|i| i.id == *dep_id) {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' does not exist in the task list."),
                });
            }
        }
    }

    let now = now_ms();
    items.push(TodoItem {
        id: id.clone(),
        description: description.to_owned(),
        status: TodoStatus::Pending,
        timeline_ms,
        depends_on: depends_on.cloned().unwrap_or_default(),
        created_at: now,
        updated_at: now,
    });

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
    })
}

fn start(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let idx = items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found."))?;

    if let Some(nag) = check_timeline(&items[idx], now_ms()) {
        nags.push(nag);
    }

    let others_in_progress: Vec<&str> = items
        .iter()
        .filter(|i| i.status == TodoStatus::InProgress && i.id != id)
        .map(|i| i.id.as_str())
        .collect();

    if !others_in_progress.is_empty() {
        return Err(format!(
            "Cannot start '{id}' — {} still in progress.",
            join_ids(&others_in_progress),
        ));
    }

    let item = &items[idx];
    for dep_id in &item.depends_on {
        let dep = items.iter().find(|i| i.id == *dep_id);
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

    items[idx].status = TodoStatus::InProgress;
    items[idx].updated_at = now_ms();

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
    })
}

fn complete(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut nags: Vec<Nag> = Vec::new();

    let idx = list
        .items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found."))?;

    let mut items = list.items.clone();
    items[idx].status = TodoStatus::Completed;
    items[idx].updated_at = now_ms();

    if let Some(nag) = check_timeline(&items[idx], items[idx].updated_at) {
        nags.push(nag);
    }

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
    })
}

fn cancel(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();

    let idx = items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found."))?;

    items[idx].status = TodoStatus::Cancelled;
    items[idx].updated_at = now_ms();

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags: Vec::new(),
    })
}

fn update(
    list: &TodoList,
    id: &str,
    description: Option<&String>,
    timeline_ms: Option<u64>,
    depends_on: Option<&Vec<String>>,
) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let idx = items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found."))?;

    if let Some(desc) = description {
        if desc.trim().is_empty() {
            return Err("Description cannot be empty.".into());
        }
        items[idx].description.clone_from(desc);
    }

    if let Some(tl) = timeline_ms {
        items[idx].timeline_ms = Some(tl);
    }

    if let Some(deps) = depends_on {
        for dep_id in deps {
            if *dep_id == id {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' is a self-reference."),
                });
            } else if !items.iter().any(|i| i.id == *dep_id) {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' does not exist in the task list."),
                });
            }
        }
        items[idx].depends_on.clone_from(deps);
    }

    let updated_at = now_ms();
    items[idx].updated_at = updated_at;

    if let Some(nag) = check_timeline(&items[idx], updated_at) {
        nags.push(nag);
    }

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
    })
}

fn remove(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let len_before = items.len();

    items.retain(|i| i.id != id);

    if items.len() == len_before {
        return Err(format!("Task '{id}' not found."));
    }

    let stale_refs: Vec<String> = items
        .iter()
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
        list: TodoList { items },
        nags,
    })
}

fn clean(list: &TodoList, keep_pending: bool) -> TodoWriteOutput {
    let items = list.items.clone();
    let (keep, removed): (Vec<_>, Vec<_>) = items.into_iter().partition(|i| match i.status {
        TodoStatus::Pending if keep_pending => true,
        TodoStatus::InProgress => true,
        _ => false,
    });

    let mut nags: Vec<Nag> = Vec::new();
    if !removed.is_empty() {
        nags.push(Nag {
            message: format!("Removed {} completed or cancelled tasks.", removed.len()),
        });
    }

    TodoWriteOutput {
        list: TodoList { items: keep },
        nags,
    }
}

fn check_timeline(item: &TodoItem, now: u64) -> Option<Nag> {
    let timeline = item.timeline_ms?;
    let elapsed = now.saturating_sub(item.created_at);
    if elapsed > timeline {
        Some(Nag {
            message: format!(
                "'{}' has exceeded its timeline ({}ms) by {}ms.",
                item.id,
                timeline,
                elapsed - timeline,
            ),
        })
    } else {
        None
    }
}

fn next_id(items: &[TodoItem]) -> String {
    let max = items
        .iter()
        .filter_map(|i| i.id.strip_prefix("task-"))
        .filter_map(|s| s.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    format!("task-{}", max + 1)
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
