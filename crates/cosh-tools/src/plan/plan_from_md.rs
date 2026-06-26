use std::fs;

use super::types::{TaskGroup, TodoItem, TodoList, TodoStatus};

/// Parse a Markdown plan file into a [`TodoList`].
///
/// Expected format:
/// ```markdown
/// ## Group Name
/// - [ ] Task description
///   - depends: task-1, task-2
/// - [x] Completed task
/// ```
pub fn plan_from_md(path: &str) -> Result<TodoList, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read plan file: {e}"))?;

    let mut groups: Vec<TaskGroup> = Vec::new();
    let mut next_id: usize = 1;

    for line in content.lines() {
        let trimmed = line.trim();

        // Task group heading
        if let Some(title) = trimmed.strip_prefix("## ") {
            groups.push(TaskGroup {
                title: title.to_owned(),
                items: Vec::new(),
            });
            continue;
        }

        if groups.is_empty() {
            continue;
        }

        let last_group = groups.last_mut().unwrap();

        // Checklist item
        if let Some(desc) = trimmed
            .strip_prefix("- [ ] ")
            .or_else(|| trimmed.strip_prefix("- [x] "))
        {
            let completed = trimmed.starts_with("- [x]");
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
        {
            if let Some(val) = rest.strip_prefix("depends:") {
                last_item.depends_on = val
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }

    Ok(TodoList { groups })
}
