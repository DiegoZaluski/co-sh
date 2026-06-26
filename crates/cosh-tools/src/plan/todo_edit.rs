use super::types::{Nag, TodoList, TodoWriteOutput};

/// Edit the metadata of an existing task.
pub struct TodoEdit {
    pub id: String,
    pub description: Option<String>,
    /// Move to a different group. `None` = leave unchanged.
    pub group: Option<String>,
    pub depends_on: Option<Vec<String>>,
}

/// Edit metadata (description, group, dependencies) of a task.
///
/// # Errors
///
/// Returns `Err` if the task does not exist or if description is empty.
pub fn todo_edit(list: &TodoList, edit: &TodoEdit) -> Result<TodoWriteOutput, String> {
    let mut groups = list.groups.clone();
    let mut nags: Vec<Nag> = Vec::new();

    let (gi, ii) = super::todo_write::find_item(&groups, &edit.id).ok_or_else(|| {
        format!("Task '{}' does not exist. Use List to see available tasks.", edit.id)
    })?;

    if let Some(desc) = &edit.description {
        if desc.trim().is_empty() {
            return Err("Description must be non-empty text.".into());
        }
        groups[gi].items[ii].description.clone_from(desc);
    }

    if let Some(deps) = &edit.depends_on {
        for dep_id in deps {
            if *dep_id == edit.id {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' is a self-reference."),
                });
            } else if !super::todo_write::item_exists(&groups, dep_id) {
                nags.push(Nag {
                    message: format!("Dependency '{dep_id}' does not exist in the task list."),
                });
            } else if super::todo_write::would_create_cycle(&groups, &edit.id, dep_id) {
                nags.push(Nag {
                    message: format!(
                        "Dependency '{dep_id}' would create a circular dependency chain."
                    ),
                });
            }
        }
        groups[gi].items[ii].depends_on.clone_from(deps);
    }

    // Move to a different group if requested.
    if let Some(new_group) = &edit.group
        && new_group != &groups[gi].title
    {
        let item = groups[gi].items.remove(ii);
        if let Some(g) = groups.iter_mut().find(|g| g.title == *new_group) {
            g.items.push(item);
        } else {
            groups.push(super::types::TaskGroup {
                title: new_group.to_owned(),
                items: vec![item],
                tests_verified: false,
            });
        }
        return Ok(TodoWriteOutput {
            list: TodoList { groups },
            nags,
        });
    }

    Ok(TodoWriteOutput {
        list: TodoList { groups },
        nags,
    })
}
