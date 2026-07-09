use std::fs;

use super::types::{PlanError, TaskGroup, TodoItem, TodoList, TodoStatus};

/// Parse a Markdown plan file into a `TodoList`.
///
/// Expected format:
/// ```markdown
/// ## Group Name
/// - [ ] Task description
///   - depends: task-1, task-2
/// - [x] Completed task
/// ```
///
/// # Errors
///
/// Returns `Err` if the file cannot be read.
#[allow(clippy::missing_panics_doc)]
pub fn plan_from_md(path: &str) -> Result<TodoList, PlanError> {
    let content = fs::read_to_string(path)
        .map_err(|e| PlanError(format!("Failed to read plan file: {e}")))?;

    let mut groups: Vec<TaskGroup> = Vec::new();
    let mut next_id: usize = 1;

    for line in content.lines() {
        let trimmed = line.trim();

        // Task group heading
        if let Some(title) = trimmed.strip_prefix("## ") {
            groups.push(TaskGroup {
                title: title.to_owned(),
                items: Vec::new(),
                tests_verified: false,
            });
            continue;
        }

        if groups.is_empty() {
            continue;
        }

        #[allow(clippy::expect_used)]
        let last_group = groups
            .last_mut()
            .expect("groups should not be empty at this point");

        // Checklist item
        if let Some(desc) = trimmed
            .strip_prefix("- [ ] ")
            .or_else(|| trimmed.strip_prefix("- [x] "))
            .or_else(|| trimmed.strip_prefix("- [X] "))
        {
            let completed = trimmed.starts_with("- [x]") || trimmed.starts_with("- [X]");
            let id = format!("task-{next_id}");
            next_id += 1;
            last_group.items.push(TodoItem {
                id,
                description: desc.to_owned(),
                status: if completed {
                    TodoStatus::Completed
                } else {
                    TodoStatus::Pending
                },
                depends_on: Vec::new(),
            });
            continue;
        }

        // Metadata sub-bullet on the last item
        if let Some(rest) = trimmed.strip_prefix("- ")
            && let Some(last_item) = last_group.items.last_mut()
            && let Some(val) = rest.strip_prefix("depends:")
        {
            last_item.depends_on = val
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    Ok(TodoList { groups })
}
