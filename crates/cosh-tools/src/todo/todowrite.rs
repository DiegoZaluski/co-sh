//! State-machine engine for the todo task list.
//!
//! This module implements the core transition logic of the todo system.
//! Each [`TodoAction`] variant is dispatched to a pure function that
//! validates invariants, mutates the list, and returns a
//! [`TodoWriteOutput`] with the new state, diagnostics (nags), and
//! optional [`PendingResolution`] records.
//!
//! # State machine
//!
//! Every task follows this lifecycle:
//!
//! ```text
//! ┌─────────┐   Start    ┌────────────┐   Complete   ┌───────────┐
//! │ Pending │ ─────────→ │ InProgress │ ───────────→ │ Completed │
//! └─────────┘            └────────────┘              └───────────┘
//!      ↓                                                ↓
//!  Cancel                                            Cancel
//!      ↓                                                ↓
//! ┌───────────┐                                   DiscardResolution
//! │ Cancelled │                                   (clears important flag)
//! └───────────┘
//!
//! ┌────────────┐  ConfirmResolution   ┌─────────────────────┐
//! │ Completed  │ ───────────────────→ │ PendingResolution   │
//! │ (important)│                      │ (returned to caller)│
//! └────────────┘                      └─────────────────────┘
//! ```
//!
//! The caller owns persistence. The tool never writes to disk — it
//! returns the updated list and leaves storage decisions to the
//! calling code.
//!
//! # Actions
//!
//! | Action | Effect |
//! |---|---|
//! | [`Add`](TodoAction::Add) | Append a new task with optional tags, timeline, and dependencies |
//! | [`Start`](TodoAction::Start) | Transition a task to `InProgress`; rejects if another is already in progress |
//! | [`Complete`](TodoAction::Complete) | Mark done; requires a `rationale` if `possibly_important` |
//! | [`Cancel`](TodoAction::Cancel) | Mark cancelled (idempotent) |
//! | [`Update`](TodoAction::Update) | Modify description, tags, timeline, or dependencies |
//! | [`Remove`](TodoAction::Remove) | Delete a task and warn about stale dependency refs |
//! | [`ConfirmResolution`](TodoAction::ConfirmResolution) | Produce a [`PendingResolution`] (after human confirmation) |
//! | [`DiscardResolution`](TodoAction::DiscardResolution) | Clear the `possibly_important` flag without recording |
//! | [`Clean`](TodoAction::Clean) | Sweep completed/cancelled tasks (optionally keep pending) |
//!
//! # Nag system
//!
//! Non-blocking diagnostics (warnings) are returned as [`Nag`] items
//! alongside every successful result. They remind the LLM to:
//!
//! - Set `possibly_important` on tasks worth preserving
//! - Check dependencies before starting work
//! - Seek human confirmation for important resolutions
//! - Watch for tasks that exceed their `timeline_ms` budget
//!
//! A nag never blocks an action; it is advisory only.
//!
//! # Errors
//!
//! The [`todowrite`] function returns `Err(String)` with a
//! human-readable explanation when invariants are violated:
//!
//! - Empty description on `Add` or `Update`
//! - Starting a non-existent task
//! - Two tasks `InProgress` concurrently
//! - Completing an important task without a `rationale`
//! - Confirming resolution for an uncompleted or non-important task
//! - Referencing a non-existent dependency
//!
//! # Examples
//!
//! ```rust
//! use cosh_tools::todo::{TodoAction, TodoList, TodoTags, todowrite};
//!
//! let list = TodoList::default();
//!
//! // Add a task
//! let output = todowrite(&list, &TodoAction::Add {
//!     description: "Refactor parser".into(),
//!     tags: Some(TodoTags { possibly_important: true }),
//!     timeline_ms: Some(86_400_000),       // 24 h
//!     depends_on: None,
//! }).unwrap();
//!
//! // Start it
//! let list = output.list;
//! let output = todowrite(&list, &TodoAction::Start { id: "task-1".into() }).unwrap();
//!
//! // Complete it with rationale
//! let list = output.list;
//! let output = todowrite(&list, &TodoAction::Complete {
//!     id: "task-1".into(),
//!     rationale: Some("Switched to nom for safer parsing".into()),
//!     resolution_tags: Some(vec!["refactor".into()]),
//! }).unwrap();
//!
//! assert_eq!(output.list.items[0].status, cosh_tools::todo::TodoStatus::Completed);
//! ```

use std::fmt::Write;

use super::types::{
    Nag, NagSeverity, PendingResolution, TodoAction, TodoItem, TodoList, TodoStatus, TodoTags,
    TodoWriteOutput, now_ms,
};

/// State-machine transitions and validation.
///
/// # Errors
///
/// Returns `Err` with a human-readable (LLM-targeted) explanation when:
/// - An `Add` action is missing a description.
/// - A `Start` targets a non-existent or already-in-progress item.
/// - A `Complete` is issued for an important item without a rationale.
/// - Dependencies are not satisfied.
pub fn todowrite(list: &TodoList, action: &TodoAction) -> Result<TodoWriteOutput, String> {
    match action {
        TodoAction::Add {
            description,
            tags,
            timeline_ms,
            depends_on,
        } => add(
            list,
            description,
            tags.as_ref(),
            *timeline_ms,
            depends_on.as_ref(),
        ),
        TodoAction::Start { id } => start(list, id),
        TodoAction::Complete {
            id,
            rationale,
            resolution_tags,
        } => complete(list, id, rationale.as_deref(), resolution_tags.as_deref()),
        TodoAction::Cancel { id } => cancel(list, id),
        TodoAction::Update {
            id,
            description,
            tags,
            timeline_ms,
            depends_on,
        } => update(
            list,
            id,
            description.as_ref(),
            tags.as_ref(),
            *timeline_ms,
            depends_on.as_ref(),
        ),
        TodoAction::Remove { id } => remove(list, id),
        TodoAction::ConfirmResolution {
            todo_id,
            rationale,
            resolution_tags,
        } => confirm_resolution(list, todo_id, rationale, resolution_tags),
        TodoAction::DiscardResolution { todo_id } => discard_resolution(list, todo_id),
        TodoAction::Clean { keep_pending } => Ok(clean(list, *keep_pending)),
    }
}

//
// Action implementations
//
fn add(
    list: &TodoList,
    description: &str,
    tags: Option<&TodoTags>,
    timeline_ms: Option<u64>,
    depends_on: Option<&Vec<String>>,
) -> Result<TodoWriteOutput, String> {
    if description.trim().is_empty() {
        return Err(
            "Cannot add a task with an empty description. Provide a meaningful description.".into(),
        );
    }

    let mut nags: Vec<Nag> = Vec::new();
    let mut items = list.items.clone();
    let id = next_id(&items);

    // Validate dependencies exist and are not self-references.
    if let Some(deps) = depends_on {
        for dep_id in deps {
            if *dep_id == id {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!(
                        "Dependency '{dep_id}' is a self-reference. A task cannot depend on itself."
                    ),
                });
            } else if !items.iter().any(|i| i.id == *dep_id) {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!("Dependency '{dep_id}' does not exist in the task list. It will be ignored until created."),
                });
            }
        }
    }
    let now = now_ms();

    items.push(TodoItem {
        id: id.clone(),
        description: description.to_owned(),
        status: TodoStatus::Pending,
        tags: tags.cloned().unwrap_or_default(),
        timeline_ms,
        depends_on: depends_on.cloned().unwrap_or_default(),
        rationale: None,
        resolution_tags: Vec::new(),
        created_at: now,
        updated_at: now,
    });

    nags.push(Nag {
        severity: NagSeverity::Warning,
        message: format!(
            "Task '{id}' added. Ensure it has appropriate tags — set \
             `possibly_important` if the rationale behind this task may be \
             worth preserving after completion."
        ),
    });

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
        pending_resolutions: Vec::new(),
    })
}

fn start(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let idx = items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| format!("Task '{id}' not found. Use `Add` to create it first."))?;

    // Check timeline enforcement.
    if let Some(nag) = check_timeline(&items[idx], now_ms()) {
        nags.push(nag);
    }

    // Validate no other task is in progress.
    let others_in_progress: Vec<&str> = items
        .iter()
        .filter(|i| i.status == TodoStatus::InProgress && i.id != id)
        .map(|i| i.id.as_str())
        .collect();

    if !others_in_progress.is_empty() {
        return Err(format!(
            "Cannot start '{id}' — {} still in progress. Complete or cancel {} first.",
            join_ids(&others_in_progress),
            join_ids(&others_in_progress),
        ));
    }

    // Validate dependencies.
    let item = &items[idx];
    for dep_id in &item.depends_on {
        let dep = items.iter().find(|i| i.id == *dep_id);
        match dep {
            Some(d) if d.status != TodoStatus::Completed => {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!(
                        "Task '{id}' depends on '{dep_id}' which is still {:?}. \
                         Consider completing it first.",
                        d.status
                    ),
                });
            }
            None => {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!(
                        "Task '{id}' depends on '{dep_id}' which no longer exists. \
                         Update the task to remove stale dependencies."
                    ),
                });
            }
            _ => {}
        }
    }

    items[idx].status = TodoStatus::InProgress;
    items[idx].updated_at = now_ms();

    nags.push(Nag {
        severity: NagSeverity::Warning,
        message: format!(
            "Task '{id}' is now in progress. Remember to set tags (especially \
             `possibly_important`) if you haven't already."
        ),
    });

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
        pending_resolutions: Vec::new(),
    })
}

fn complete(
    list: &TodoList,
    id: &str,
    rationale: Option<&str>,
    resolution_tags: Option<&[String]>,
) -> Result<TodoWriteOutput, String> {
    let mut nags: Vec<Nag> = Vec::new();

    // Find the item and clone what we need before mutating.
    let (idx, was_important) = {
        let pos = list
            .items
            .iter()
            .position(|i| i.id == id)
            .ok_or_else(|| format!("Task '{id}' not found."))?;
        let imp = list.items[pos].tags.possibly_important;
        (pos, imp)
    };

    // Require rationale for important tasks.
    if was_important && rationale.is_none_or(|r| r.trim().is_empty()) {
        return Err(format!(
            "Task '{id}' is tagged as `possibly_important`. \
             Provide a `rationale` explaining \
             how/why this task was resolved the way it was (e.g., which \
             algorithm was chosen and why, which library was preferred, \
             architectural decisions made)."
        ));
    }

    let mut items = list.items.clone();
    items[idx].status = TodoStatus::Completed;
    items[idx].rationale = rationale.map(String::from);
    items[idx].resolution_tags = resolution_tags.map(Vec::from).unwrap_or_default();
    items[idx].updated_at = now_ms();

    // Check timeline enforcement.
    if let Some(nag) = check_timeline(&items[idx], items[idx].updated_at) {
        nags.push(nag);
    }

    // Nag about important tasks needing human confirmation.
    if was_important {
        nags.push(Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Task '{id}' was tagged as `possibly_important` and is now completed. \
                 Ask your user to confirm the resolution was successful. \
                 If they confirm, call `ConfirmResolution` with the rationale \
                 to produce a persistent resolution record. If not, call \
                 `DiscardResolution` to silence this reminder."
            ),
        });
    }

    nags.push(Nag {
        severity: NagSeverity::Warning,
        message: format!(
            "Task '{id}' completed. Double-check that all related tasks are \
             also updated and that no loose ends remain."
        ),
    });

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
        pending_resolutions: Vec::new(),
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
        nags: vec![Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Task '{id}' was cancelled. Make sure the reason is understood and no blockers remain."
            ),
        }],
        pending_resolutions: Vec::new(),
    })
}

fn update(
    list: &TodoList,
    id: &str,
    description: Option<&String>,
    tags: Option<&TodoTags>,
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
    if let Some(t) = tags {
        items[idx].tags = t.clone();
    }
    if let Some(tl) = timeline_ms {
        items[idx].timeline_ms = Some(tl);
    }
    if let Some(deps) = depends_on {
        for dep_id in deps {
            if *dep_id == id {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!(
                        "Dependency '{dep_id}' is a self-reference. A task cannot depend on itself."
                    ),
                });
            } else if !items.iter().any(|i| i.id == *dep_id) {
                nags.push(Nag {
                    severity: NagSeverity::Warning,
                    message: format!("Dependency '{dep_id}' does not exist in the task list. It will be ignored until created."),
                });
            }
        }
        items[idx].depends_on.clone_from(deps);
    }

    let updated_at = now_ms();
    items[idx].updated_at = updated_at;

    // Check timeline enforcement.
    if let Some(nag) = check_timeline(&items[idx], updated_at) {
        nags.push(nag);
    }

    if !items[idx].tags.possibly_important
        && items[idx].timeline_ms.is_none()
        && items[idx].depends_on.is_empty()
    {
        nags.push(Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Task '{id}' has no tags, no timeline, and no dependencies. \
                 Consider setting `possibly_important` if this task might have \
                 long-term value worth preserving."
            ),
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
        pending_resolutions: Vec::new(),
    })
}

fn remove(list: &TodoList, id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let len_before = items.len();

    items.retain(|i| i.id != id);

    if items.len() == len_before {
        return Err(format!("Task '{id}' not found."));
    }

    // Check if any remaining task references the removed ID.
    let stale_refs: Vec<String> = items
        .iter()
        .filter(|i| i.depends_on.contains(&id.to_string()))
        .map(|i| i.id.clone())
        .collect();

    let mut nags = Vec::new();
    if !stale_refs.is_empty() {
        nags.push(Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Task '{id}' was removed but is still referenced as a dependency \
                 by {}. Update those tasks to remove stale dependencies.",
                join_ids(&stale_refs.iter().map(String::as_str).collect::<Vec<_>>()),
            ),
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags,
        pending_resolutions: Vec::new(),
    })
}

fn confirm_resolution(
    list: &TodoList,
    todo_id: &str,
    rationale: &str,
    resolution_tags: &[String],
) -> Result<TodoWriteOutput, String> {
    let item = list
        .items
        .iter()
        .find(|i| i.id == todo_id)
        .ok_or_else(|| format!("Task '{todo_id}' not found."))?;

    if item.status != TodoStatus::Completed {
        return Err(format!(
            "Cannot confirm resolution for '{todo_id}' — the task is not completed. \
             Complete it first."
        ));
    }

    if !item.tags.possibly_important {
        return Err(format!(
            "Task '{todo_id}' was not tagged as `possibly_important`. \
             Only important tasks produce resolutions. Use `Update` to set the \
             tag if this was an oversight."
        ));
    }

    if rationale.trim().is_empty() {
        return Err(format!(
            "Cannot confirm resolution for '{todo_id}' — rationale is empty. \
             Provide an explanation of why the task was resolved this way."
        ));
    }

    let resolution = PendingResolution {
        todo_id: todo_id.to_owned(),
        description: item.description.clone(),
        rationale: rationale.to_owned(),
        tags: resolution_tags.to_vec(),
    };

    Ok(TodoWriteOutput {
        list: list.clone(),
        nags: vec![Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Resolution for '{todo_id}' confirmed. The caller should now \
                 persist the `PendingResolution` record — the tool does not \
                 store it automatically."
            ),
        }],
        pending_resolutions: vec![resolution],
    })
}

fn discard_resolution(list: &TodoList, todo_id: &str) -> Result<TodoWriteOutput, String> {
    let mut items = list.items.clone();
    let item = items
        .iter_mut()
        .find(|i| i.id == todo_id)
        .ok_or_else(|| format!("Task '{todo_id}' not found."))?;

    if item.status != TodoStatus::Completed {
        return Err(format!(
            "Task '{todo_id}' is not completed. Nothing to discard."
        ));
    }

    item.tags.possibly_important = false;

    Ok(TodoWriteOutput {
        list: TodoList { items },
        nags: vec![],
        pending_resolutions: vec![],
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
            severity: NagSeverity::Warning,
            message: format!("Removed {} completed/cancelled tasks.", removed.len()),
        });

        let pending_resolution_count = removed
            .iter()
            .filter(|i| i.tags.possibly_important && i.status == TodoStatus::Completed)
            .count();

        if pending_resolution_count > 0 {
            nags.push(Nag {
                severity: NagSeverity::Warning,
                message: format!(
                    "{pending_resolution_count} removed task(s) had `possibly_important` set and may need resolution confirmation. \
                     Call `ConfirmResolution` or `DiscardResolution` before cleaning to avoid losing records."
                ),
            });
        }
    }

    TodoWriteOutput {
        list: TodoList { items: keep },
        nags,
        pending_resolutions: Vec::new(),
    }
}

// Internal helpers

/// Check if a task has exceeded its `timeline_ms` budget and return a nag.
fn check_timeline(item: &TodoItem, now: u64) -> Option<Nag> {
    let timeline = item.timeline_ms?;
    let elapsed = now.saturating_sub(item.created_at);
    if elapsed > timeline {
        Some(Nag {
            severity: NagSeverity::Warning,
            message: format!(
                "Task '{}' has exceeded its timeline ({}ms) by {}ms.",
                item.id,
                timeline,
                elapsed - timeline,
            ),
        })
    } else {
        None
    }
}

/// Produce a unique id based on existing items.
///
/// Scans existing IDs for the highest `task-N` suffix and assigns
/// `task-{N+1}`, avoiding collisions when items are removed and re-added.
fn next_id(items: &[TodoItem]) -> String {
    let max = items
        .iter()
        .filter_map(|i| i.id.strip_prefix("task-"))
        .filter_map(|s| s.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    format!("task-{}", max + 1)
}

/// Join a list of IDs into a human-readable string.
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
