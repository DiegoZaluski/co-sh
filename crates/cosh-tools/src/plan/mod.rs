pub mod todo_cross_off;
pub mod todo_edit;
pub mod todo_read;
pub mod todo_write;
pub mod types;

#[cfg(test)]
mod test;

pub use todo_cross_off::{TodoCrossOff, todo_cross_off};
pub use todo_edit::{TodoEdit, todo_edit};
pub use todo_read::todo_read;
pub use todo_write::{TodoWriteAction, todo_write};
pub use types::{
    Nag, PlanError, ReplaceGroup, ReplaceItem, TaskGroup, TodoCrossOffInput, TodoEditInput,
    TodoItem, TodoList, TodoReadAction, TodoReadInput, TodoReadOutput, TodoStatus, TodoWriteInput,
    TodoWriteOutput,
};

use crate::ToolDescription;

// NOTE: the old PLAN_WRITE constant (Markdown plan-file workflow) was removed.
// The structured `ReplaceList` action on `plan_todo_write` is now the canonical
// way to create a complete TODO list in a single call.

/// Plan instructs the model on how to write plans and provides
/// stateful wrappers over the pure todo operations.
pub struct Plan {
    list: TodoList,
    with_test: bool,

    /// MCP Tool description for `todo_write`.
    pub description_todo_write: ToolDescription,
    /// MCP Tool description for `todo_edit`.
    pub description_todo_edit: ToolDescription,
    /// MCP Tool description for `todo_cross_off`.
    pub description_todo_cross_off: ToolDescription,
    /// MCP Tool description for `todo_read`.
    pub description_todo_read: ToolDescription,
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

impl Plan {
    /// Creates an empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self {
            list: TodoList::default(),
            with_test: false,
            description_todo_write: serde_json::json!({
                "name": "plan_todo_write",
                "description": concat!(
                    "Mutate the TODO list by adding, starting, removing, cleaning, ",
                    "or verifying tasks. To create the full plan up front, prefer a ",
                    "single `ReplaceList` call with all groups and tasks (use the ",
                    "optional per-item `key` so sibling tasks can reference each other ",
                    "in `depends_on` within the same call). Use `Add` for incremental ",
                    "tasks during execution, `Start` to mark one in-progress, `Remove` ",
                    "to delete, `Clean` to drop completed/cancelled tasks, and ",
                    "`VerifyGroup` to confirm a completed group was tested."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "object",
                            "description": "The mutation action to perform",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "ReplaceList" },
                                        "groups": {
                                            "type": "array",
                                            "description": "Complete group/task list replacing the current TODO state. IDs are assigned as task-1..N in listed order.",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "title": { "type": "string", "description": "The group name" },
                                                    "items": {
                                                        "type": "array",
                                                        "items": {
                                                            "type": "object",
                                                            "properties": {
                                                                "key": { "type": "string", "description": "Optional alias other tasks in this same call reference in depends_on; resolved to the real task-N id" },
                                                                "description": { "type": "string", "description": "Task description" },
                                                                "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Sibling keys or task ids this task depends on" }
                                                            },
                                                            "required": ["description"]
                                                        }
                                                    }
                                                },
                                                "required": ["title", "items"]
                                            }
                                        }
                                    },
                                    "required": ["type", "groups"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Add" },
                                        "group": { "type": "string", "description": "The task group name" },
                                        "description": { "type": "string", "description": "Task description" },
                                        "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Optional task IDs this task depends on" }
                                    },
                                    "required": ["type", "group", "description"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Start" },
                                        "id": { "type": "string", "description": "Task ID to mark as in-progress" }
                                    },
                                    "required": ["type", "id"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Remove" },
                                        "id": { "type": "string", "description": "Task ID to remove" }
                                    },
                                    "required": ["type", "id"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Clean" },
                                        "keep_pending": { "type": "boolean", "description": "If true, only remove completed/cancelled tasks; if false, remove all" }
                                    },
                                    "required": ["type", "keep_pending"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "VerifyGroup" },
                                        "group": { "type": "string", "description": "Group name to verify tests for" }
                                    },
                                    "required": ["type", "group"]
                                }
                            ]
                        }
                    },
                    "required": ["action"]
                }
            }),
            description_todo_edit: serde_json::json!({
                "name": "plan_todo_edit",
                "description": concat!(
                    "Edit an existing task's metadata: description, group assignment, ",
                    "or dependency list. Only the provided fields are updated; ",
                    "omitted fields remain unchanged."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "edit": {
                            "type": "object",
                            "description": "The edit operation to perform",
                            "properties": {
                                "id": { "type": "string", "description": "ID of the task to edit" },
                                "description": { "type": "string", "description": "Optional new description for the task" },
                                "group": { "type": "string", "description": "Optional new group name to move the task to" },
                                "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Optional new dependency list" }
                            },
                            "required": ["id"]
                        }
                    },
                    "required": ["edit"]
                }
            }),
            description_todo_cross_off: serde_json::json!({
                "name": "plan_todo_cross_off",
                "description": concat!(
                    "Mark a task as completed or cancelled. Updates the task status ",
                    "to 'Completed' or 'Cancelled' and refreshes the internal state."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "object",
                            "description": "The cross-off action",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Complete" },
                                        "id": { "type": "string", "description": "Task ID to mark as completed" }
                                    },
                                    "required": ["type", "id"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Cancel" },
                                        "id": { "type": "string", "description": "Task ID to cancel" }
                                    },
                                    "required": ["type", "id"]
                                }
                            ]
                        }
                    },
                    "required": ["action"]
                }
            }),
            description_todo_read: serde_json::json!({
                "name": "plan_todo_read",
                "description": concat!(
                    "Query the TODO list state. Supports listing all tasks with ",
                    "optional filtering by group and/or status, or fetching a ",
                    "single task by its ID. Returns the matching task groups, ",
                    "items, and any nag messages for the user."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "object",
                            "description": "The read action to perform",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "List" },
                                        "group": { "type": "string", "description": "Optional group name filter" },
                                        "status": { "type": "string", "description": "Optional status filter (Pending, InProgress, Completed, Cancelled)" }
                                    },
                                    "required": ["type"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "const": "Get" },
                                        "id": { "type": "string", "description": "Task ID to fetch" }
                                    },
                                    "required": ["type", "id"]
                                }
                            ]
                        }
                    },
                    "required": ["action"]
                }
            }),
        }
    }

    /// Returns a reference to the internal todo list.
    #[must_use]
    pub const fn list(&self) -> &TodoList {
        &self.list
    }

    /// Restore the structured projection reconstructed from session history.
    pub fn restore_list(&mut self, list: TodoList) {
        self.list = list;
    }

    /// Apply a mutation action and update internal state.
    /// See `todo_write` for available actions and error conditions.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the underlying `todo_write` operation fails.
    pub fn todo_write(&mut self, action: &TodoWriteAction) -> Result<TodoWriteOutput, PlanError> {
        let output = todo_write(&self.list, action)?;
        self.list = output.list.clone();
        Ok(output)
    }

    /// Edit a task and update internal state.
    /// See `todo_edit` for available fields and error conditions.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the underlying `todo_edit` operation fails.
    pub fn todo_edit(&mut self, edit: &TodoEdit) -> Result<TodoWriteOutput, PlanError> {
        let output = todo_edit(&self.list, edit)?;
        self.list = output.list.clone();
        Ok(output)
    }

    /// Complete or cancel a task and update internal state.
    /// See `todo_cross_off` for available actions and error conditions.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the underlying `todo_cross_off` operation fails.
    pub fn todo_cross_off(&mut self, action: &TodoCrossOff) -> Result<TodoWriteOutput, PlanError> {
        let output = todo_cross_off(&self.list, action)?;
        self.list = output.list.clone();
        Ok(output)
    }

    /// Query the todo list.
    ///
    /// # Errors
    ///
    /// See `todo_read` for available actions and error conditions.
    pub fn todo_read(&self, action: &TodoReadAction) -> Result<TodoReadOutput, PlanError> {
        todo_read(&self.list, action)
    }

    /// Set whether test generation is expected after each completed group.
    /// Default is `false` (no test expectation).
    pub const fn with_test(&mut self, value: bool) -> &mut Self {
        self.with_test = value;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Plan;

    #[test]
    fn with_test_default_is_false() {
        let plan = Plan::new();
        assert!(!plan.with_test);
    }

    #[test]
    fn with_test_setter_changes_value() {
        let mut plan = Plan::new();
        assert!(!plan.with_test);
        plan.with_test(true);
        assert!(plan.with_test);
        plan.with_test(false);
        assert!(!plan.with_test);
    }

    #[test]
    fn with_test_allows_chaining() {
        let mut plan = Plan::new();
        plan.with_test(true).with_test(false);
        assert!(!plan.with_test);
    }
}
