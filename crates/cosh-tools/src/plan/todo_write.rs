use std::fmt::Write;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::types::{Nag, PlanError, ReplaceGroup, TodoItem, TodoList, TodoStatus, TodoWriteOutput};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TodoWriteAction {
    Add {
        group: String,
        description: String,
        depends_on: Option<Vec<String>>,
    },
    Start {
        id: String,
    },
    Remove {
        id: String,
    },
    Clean {
        keep_pending: bool,
    },
    /// Confirm that tests were run for all completed tasks in the given group.
    VerifyGroup {
        group: String,
    },
    /// Replace the entire TODO list with the given groups in a single call.
    /// This is the preferred way to create a complete plan up front.
    ReplaceList {
        groups: Vec<ReplaceGroup>,
    },
}

/// Apply a mutation action to the todo list.
///
/// Actions:
/// - `ReplaceList { groups }` — replace the whole list at once with complete
///   groups/tasks; the preferred way to create a full plan in a single call.
///   Items may carry an optional `key` alias that siblings reference in
///   `depends_on`; the tool resolves aliases to the assigned `task-N` ids.
/// - `Add { group, description, depends_on? }` — append a new task
/// - `Start { id }` — mark a task as in-progress (only one at a time)
/// - `Remove { id }` — delete a task
/// - `Clean { keep_pending }` — remove completed/cancelled tasks and empty groups
/// - `VerifyGroup { group }` — confirm tests were run for a completed group
///
/// # Errors
///
/// Returns `Err` with an explanation when:
/// - A `ReplaceList` has no groups, an empty description, or duplicate keys.
/// - An `Add` action has an empty description.
/// - A `Start` targets a non-existent or already-in-progress item.
/// - Dependencies are not satisfied.
/// - `VerifyGroup` targets a non-existent group.
pub fn todo_write(list: &TodoList, action: &TodoWriteAction) -> Result<TodoWriteOutput, PlanError> {
    match action {
        TodoWriteAction::Add {
            group,
            description,
            depends_on,
        } => add(list, group, description, depends_on.as_ref()),
        TodoWriteAction::ReplaceList { groups } => replace_list(list, groups),
        TodoWriteAction::Start { id } => start(list, id),
        TodoWriteAction::Remove { id } => remove(list, id),
        TodoWriteAction::Clean { keep_pending } => Ok(clean(list, *keep_pending)),
        TodoWriteAction::VerifyGroup { group } => verify_group(list, group),
    }
}

fn add(
    list: &TodoList,
    group: &str,
    description: &str,
    depends_on: Option<&Vec<String>>,
) -> Result<TodoWriteOutput, PlanError> {
    if description.trim().is_empty() {
        return Err(PlanError("Description must be non-empty text.".into()));
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
        id,
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
            tests_verified: false,
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}

fn start(list: &TodoList, id: &str) -> Result<TodoWriteOutput, PlanError> {
    let mut groups = list.groups.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let (gi, ii) = find_item(&groups, id).ok_or_else(|| {
        PlanError(format!(
            "Task '{id}' does not exist. Use List to see available tasks."
        ))
    })?;

    let others_in_progress: Vec<&str> = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| i.status == TodoStatus::InProgress && i.id != id)
        .map(|i| i.id.as_str())
        .collect();

    if !others_in_progress.is_empty() {
        return Err(PlanError(format!(
            "Only one task at a time can be InProgress. Complete or cancel {} first before starting '{id}'.",
            join_ids(&others_in_progress),
        )));
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
                    message: format!("'{id}' depends on '{dep_id}' which no longer exists."),
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

fn remove(list: &TodoList, id: &str) -> Result<TodoWriteOutput, PlanError> {
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
        return Err(PlanError(format!(
            "Task '{id}' does not exist. Use List to see available tasks."
        )));
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

    let groups_before = groups.len();
    groups.retain(|g| !g.items.is_empty());
    let groups_removed = groups_before - groups.len();

    let mut nags: Vec<Nag> = Vec::new();
    if total_removed > 0 {
        nags.push(Nag {
            message: format!("Removed {total_removed} completed or cancelled tasks."),
        });
    }
    if groups_removed > 0 {
        nags.push(Nag {
            message: format!(
                "Removed {groups_removed} empty group{}.",
                if groups_removed == 1 { "" } else { "s" }
            ),
        });
    }

    TodoWriteOutput {
        list: TodoList { groups },
        nags,
    }
}

fn verify_group(list: &TodoList, group: &str) -> Result<TodoWriteOutput, PlanError> {
    let mut groups = list.groups.clone();
    let g = groups
        .iter_mut()
        .find(|g| g.title == group)
        .ok_or_else(|| PlanError(format!("Group '{group}' does not exist.")))?;

    g.tests_verified = true;

    let mut nags = Vec::new();
    let has_pending = g
        .items
        .iter()
        .any(|i| matches!(i.status, TodoStatus::Pending | TodoStatus::InProgress));
    if has_pending {
        nags.push(Nag {
            message: format!("Group '{group}' still has pending or in-progress tasks."),
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}

fn replace_list(
    list: &TodoList,
    new_groups: &[ReplaceGroup],
) -> Result<TodoWriteOutput, PlanError> {
    let mut nags: Vec<Nag> = Vec::new();

    // Nag when overwriting a non-empty list (destructive operation).
    let existing_count: usize = list.groups.iter().map(|g| g.items.len()).sum();
    if existing_count > 0 {
        nags.push(Nag {
            message: format!(
                "Replaced the existing TODO list ({existing_count} task{}). Previous tasks are gone.",
                if existing_count == 1 { "" } else { "s" }
            ),
        });
    }

    if new_groups.is_empty() {
        return Err(PlanError(
            "ReplaceList requires at least one group. Use Clean to empty the list instead.".into(),
        ));
    }

    // Validate descriptions up front (fail fast, before mutating anything).
    for g in new_groups {
        for (idx, item) in g.items.iter().enumerate() {
            if item.description.trim().is_empty() {
                return Err(PlanError(format!(
                    "Description must be non-empty text (group '{}', item {}).",
                    g.title,
                    idx + 1
                )));
            }
        }
    }

    // Collect sibling keys and validate uniqueness. Keys are resolved to the
    // ids assigned below, allowing intra-batch dependencies. Keys that look
    // like real ids (task-N) are rejected: they would shadow the id assigned
    // to an actual task and silently redirect dependencies.
    let mut key_to_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    {
        let mut key_next_id: usize = 1;
        for g in new_groups {
            for item in &g.items {
                let id = format!("task-{key_next_id}");
                key_next_id += 1;
                if let Some(key) = &item.key {
                    if key.starts_with("task-") && key[5..].bytes().all(|b| b.is_ascii_digit()) && !key[5..].is_empty() {
                        return Err(PlanError(format!(
                            "Key '{key}' looks like a task id; keys must not be 'task-<number>' to avoid shadowing real ids."
                        )));
                    }
                    if key_to_id.insert(key.clone(), id).is_some() {
                        return Err(PlanError(format!(
                            "Duplicate key '{key}' in ReplaceList; keys must be unique."
                        )));
                    }
                }
            }
        }
    }

    // Assign ids sequentially across all groups (the list was replaced,
    // so numbering starts at 1).
    let mut groups: Vec<super::types::TaskGroup> = Vec::with_capacity(new_groups.len());
    let mut next_id: usize = 1;
    for g in new_groups {
        let mut items = Vec::with_capacity(g.items.len());
        for item in &g.items {
            let id = format!("task-{next_id}");
            next_id += 1;
            items.push(TodoItem {
                id,
                description: item.description.clone(),
                status: TodoStatus::Pending,
                depends_on: Vec::new(),
            });
        }
        groups.push(super::types::TaskGroup {
            title: g.title.clone(),
            items,
            tests_verified: false,
        });
    }

    // Resolve depends_on: sibling `key` -> task-N, or an existing task id.
    let all_ids: Vec<String> = groups
        .iter()
        .flat_map(|g| &g.items)
        .map(|i| i.id.clone())
        .collect();
    let mut item_idx = 0usize;
    for g in new_groups {
        for item in &g.items {
            if let Some(deps) = &item.depends_on {
                let mut resolved: Vec<String> = Vec::with_capacity(deps.len());
                for dep in deps {
                    if let Some(id) = key_to_id.get(dep) {
                        if Some(dep.as_str()) == item.key.as_deref() {
                            nags.push(Nag {
                                message: format!("Dependency '{dep}' is a self-reference."),
                            });
                            continue;
                        }
                        resolved.push(id.clone());
                    } else if all_ids.iter().any(|id| id == dep) {
                        resolved.push(dep.clone());
                    } else {
                        nags.push(Nag {
                            message: format!(
                                "Dependency '{dep}' is neither a sibling key nor an existing task id."
                            ),
                        });
                    }
                }
                groups
                    .iter_mut()
                    .flat_map(|g| &mut g.items)
                    .nth(item_idx)
                    .expect("item index in range")
                    .depends_on = resolved;
            }
            item_idx += 1;
        }
    }

    // Cycle detection. Resolved deps always exist by construction (they come
    // from `key_to_id` or matched `all_ids` above), so only cycles remain to
    // check. One nag per cycle: items already seen as part of a cycle are
    // skipped, so a k-item cycle reports once, not k times.
    let mut cycle_reported: Vec<String> = Vec::new();
    for g in &groups {
        for i in &g.items {
            if cycle_reported.contains(&i.id) {
                continue;
            }
            if would_create_cycle(&groups, &i.id, &i.id) {
                nags.push(Nag {
                    message: format!(
                        "Dependencies of '{}' form a circular dependency chain.",
                        i.id
                    ),
                });
                // Mark every member of this cycle so it is reported once.
                let mut queue = vec![i.id.clone()];
                let mut seen: Vec<String> = vec![i.id.clone()];
                while let Some(current) = queue.pop() {
                    for gi in &groups {
                        for it in &gi.items {
                            if it.id == current {
                                for dep in &it.depends_on {
                                    if !seen.contains(dep) {
                                        seen.push(dep.clone());
                                        queue.push(dep.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                cycle_reported.extend(seen);
            }
        }
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
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

/// Check if adding `dep_id` as a dependency of `id` would create a cycle.
/// Returns `true` if `dep_id` already transitively depends on `id`
/// (i.e., there's a path `dep_id → … → id` in the dependency graph).
pub(super) fn would_create_cycle(
    groups: &[super::types::TaskGroup],
    id: &str,
    dep_id: &str,
) -> bool {
    let mut visited = vec![dep_id.to_owned()];
    let mut queue = vec![dep_id.to_owned()];
    while let Some(current) = queue.pop() {
        for g in groups {
            for item in &g.items {
                if item.id == current {
                    for dep in &item.depends_on {
                        if dep == id {
                            return true;
                        }
                        if !visited.contains(dep) {
                            visited.push(dep.clone());
                            queue.push(dep.clone());
                        }
                    }
                }
            }
        }
    }
    false
}

pub(super) fn find_item_in_groups<'a>(
    groups: &'a [super::types::TaskGroup],
    id: &str,
) -> Option<&'a TodoItem> {
    groups.iter().flat_map(|g| &g.items).find(|i| i.id == id)
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
