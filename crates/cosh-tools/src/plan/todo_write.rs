use super::types::{
    Nag, PlanError, TodoItem, TodoItemInput, TodoList, TodoStatus, TodoWriteOutput,
};

/// Replace the entire TODO list with the given flat list of tasks
/// (full-state write, the whole list every call).
///
/// Ids are assigned as `task-1..N` in listed order. Items may carry an
/// optional `key` alias that siblings reference in `depends_on`; the function
/// resolves aliases to the assigned `task-N` ids (existing `task-N` ids are
/// accepted too). An empty list clears the TODO state.
///
/// # Errors
///
/// Returns `Err` with an explanation when:
/// - An item has an empty description, or keys are duplicated or shaped
///   like `task-<number>`.
/// - More than one task is marked as in-progress.
pub fn todo_write(todos: &[TodoItemInput]) -> Result<TodoWriteOutput, PlanError> {
    let mut nags: Vec<Nag> = Vec::new();

    // Validate descriptions up front (fail fast, before mutating anything).
    for (idx, item) in todos.iter().enumerate() {
        if item.description.trim().is_empty() {
            return Err(PlanError(format!(
                "Description must be non-empty text (item {}).",
                idx + 1
            )));
        }
    }

    // Collect keys and validate uniqueness. Keys are resolved to the ids
    // assigned below, allowing intra-batch dependencies. Keys that look like
    // real ids (task-N) are rejected: they would shadow the id assigned to
    // an actual task and silently redirect dependencies.
    let mut key_to_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (idx, item) in todos.iter().enumerate() {
        let id = format!("task-{}", idx + 1);
        if let Some(key) = &item.key {
            if key.starts_with("task-")
                && key[5..].bytes().all(|b| b.is_ascii_digit())
                && !key[5..].is_empty()
            {
                return Err(PlanError(format!(
                    "Key '{key}' looks like a task id; keys must not be 'task-<number>' to avoid shadowing real ids."
                )));
            }
            if key_to_id.insert(key.clone(), id.clone()).is_some() {
                return Err(PlanError(format!(
                    "Duplicate key '{key}'; keys must be unique."
                )));
            }
        }
    }

    // Exactly one task may be in progress at a time.
    let in_progress: Vec<usize> = todos
        .iter()
        .enumerate()
        .filter(|(_, i)| i.status == TodoStatus::InProgress)
        .map(|(idx, _)| idx)
        .collect();
    if in_progress.len() > 1 {
        let ids = in_progress
            .iter()
            .map(|idx| format!("'task-{}'", idx + 1))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PlanError(format!(
            "Only one task at a time can be in_progress; got {ids}."
        )));
    }

    // Assign ids sequentially in listed order.
    let items: Vec<TodoItem> = todos
        .iter()
        .enumerate()
        .map(|(idx, item)| TodoItem {
            id: format!("task-{}", idx + 1),
            description: item.description.clone(),
            status: item.status,
            depends_on: Vec::new(),
        })
        .collect();

    // Resolve depends_on: sibling `key` -> task-N, or an existing task id.
    let all_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let mut resolved_items = Vec::with_capacity(items.len());
    for (idx, item) in todos.iter().enumerate() {
        let mut resolved: Vec<String> =
            Vec::with_capacity(item.depends_on.as_ref().map_or(0, Vec::len));
        for dep in item.depends_on.as_ref().into_iter().flatten() {
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
        let mut out = items[idx].clone();
        out.depends_on = resolved;
        resolved_items.push(out);
    }

    // Cycle detection. Resolved deps always exist by construction (they come
    // from `key_to_id` or matched `all_ids` above), so only cycles remain to
    // check. One nag per cycle: items already seen as part of a cycle are
    // skipped, so a k-item cycle reports once, not k times.
    let mut cycle_reported: Vec<String> = Vec::new();
    for i in &resolved_items {
        if cycle_reported.contains(&i.id) {
            continue;
        }
        if would_create_cycle(&resolved_items, &i.id, &i.id) {
            nags.push(Nag {
                message: format!(
                    "Dependencies of '{}' form a circular dependency chain.",
                    i.id
                ),
            });
            // Mark exactly the members of this cycle (nodes reachable from
            // `i` that can reach `i` back) so it is reported once, not once
            // per member — without swallowing downstream unrelated cycles.
            let reachable = reachable_from(&resolved_items, &i.id);
            let members: Vec<String> = reachable
                .iter()
                .filter(|n| would_create_cycle(&resolved_items, &i.id, n))
                .cloned()
                .collect();
            cycle_reported.extend(members);
        }
    }

    // A task marked in-progress whose dependencies are not completed nags
    // (dependencies are advisory; the state is accepted, the model is told).
    for i in &resolved_items {
        if i.status != TodoStatus::InProgress {
            continue;
        }
        for dep_id in &i.depends_on {
            if let Some(d) = resolved_items.iter().find(|d| &d.id == dep_id)
                && d.status != TodoStatus::Completed
            {
                nags.push(Nag {
                    message: format!(
                        "'{}' depends on '{dep_id}' which is still {}.",
                        i.id, d.status
                    ),
                });
            }
        }
    }

    Ok(TodoWriteOutput {
        list: TodoList {
            items: resolved_items,
        },
        nags,
    })
}

/// Check if `dep_id` already transitively depends on `id`
/// (i.e., there's a path `dep_id → … → id` in the dependency graph).
pub(super) fn would_create_cycle(items: &[TodoItem], id: &str, dep_id: &str) -> bool {
    let mut visited = vec![dep_id.to_owned()];
    let mut queue = vec![dep_id.to_owned()];
    while let Some(current) = queue.pop() {
        for item in items {
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
    false
}

/// All item ids reachable from `start` via `depends_on` edges, `start` included.
fn reachable_from(items: &[TodoItem], start: &str) -> Vec<String> {
    let mut seen: Vec<String> = vec![start.to_owned()];
    let mut queue = vec![start.to_owned()];
    while let Some(current) = queue.pop() {
        for it in items {
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
    seen
}
