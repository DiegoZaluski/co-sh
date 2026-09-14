pub mod todo_write;
pub mod types;

#[cfg(test)]
mod test;

pub use todo_write::todo_write;
pub use types::{
    Nag, PlanError, TodoItem, TodoItemInput, TodoList, TodoStatus, TodoWriteInput, TodoWriteOutput,
};

use crate::ToolDescription;

/// Plan instructs the model on how to write plans and provides a
/// stateful wrapper over the pure todo operation.
pub struct Plan {
    list: TodoList,

    /// MCP Tool description for `todo_write`.
    pub description_todo_write: ToolDescription,
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
            description_todo_write: serde_json::json!({
                "name": "plan_todo_write",
                "description": concat!(
                    "Write the full TODO list. This replaces the previous list ",
                    "entirely: send ALL tasks on every call, including unchanged ",
                    "ones. To mark a task in_progress, completed, or cancelled, ",
                    "rewrite the whole list with the updated status. Exactly one ",
                    "task may be in_progress at a time. Ids (task-1..N) are ",
                    "assigned in listed order; use the optional per-item `key` so ",
                    "sibling tasks can reference each other in `depends_on` within ",
                    "the same call. Call with an empty list to clear the plan.\n",
                    "Example:\n",
                    "{\"todos\": [{\"description\": \"install deps\", \"key\": \"deps\"}, ",
                    "{\"description\": \"write tests\", \"status\": \"pending\", ",
                    "\"depends_on\": [\"deps\"]}]}"
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The complete task list, replacing the current TODO state. Ids are assigned as task-1..N in listed order.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "key": { "type": "string", "description": "Optional alias other tasks in this same call reference in depends_on; resolved to the real task-N id" },
                                    "description": { "type": "string", "description": "Task description" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"], "description": "Defaults to pending when omitted. Exactly one task may be in_progress." },
                                    "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Sibling keys or task ids this task depends on" }
                                },
                                "required": ["description"]
                            }
                        }
                    },
                    "required": ["todos"]
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

    /// Replace the whole todo list with the submitted one.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the underlying `todo_write` operation fails.
    pub fn todo_write(&mut self, todos: &[TodoItemInput]) -> Result<TodoWriteOutput, PlanError> {
        let output = todo_write(todos)?;
        self.list = output.list.clone();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::Plan;

    #[test]
    fn new_plan_has_an_empty_list() {
        let plan = Plan::new();
        assert!(plan.list().items.is_empty());
    }
}
