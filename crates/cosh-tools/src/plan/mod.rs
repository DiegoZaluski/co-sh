pub mod plan_from_md;
pub mod todo_cross_off;
pub mod todo_edit;
pub mod todo_read;
pub mod todo_write;
pub mod types;

#[cfg(test)]
mod test;

pub use plan_from_md::plan_from_md;
pub use todo_cross_off::{TodoCrossOff, todo_cross_off};
pub use todo_edit::{TodoEdit, todo_edit};
pub use todo_read::todo_read;
pub use todo_write::{TodoWriteAction, todo_write};
pub use types::{
    Nag, PlanError, TaskGroup, TodoItem, TodoList, TodoReadAction, TodoReadOutput, TodoStatus,
    TodoWriteOutput,
};

/// Static prompt injected when the model uses the plan tool.
/// Teaches the model the plan file syntax so the file can be
/// automatically parsed into a structured TODO list.
pub const PLAN_WRITE: &str = "\
# Plan: <title>

Write your plan using standard Markdown with flat checklist items.
Every plan file must follow this exact structure to be parseable.

## Group heading (required — one per group of tasks)
- [ ] Task description
  - depends: task-<N>, task-<N>   (optional, comma-separated)
- [ ] Another task
- [x] Completed task (optional)

## Another group
- [ ] More tasks

Rules:
1. Start each group with `## ` (level-2 heading). The text after `## ` becomes the group title.
2. Each task is a checklist item: `- [ ] ` for pending, `- [x] ` for completed.
3. `depends:` goes on a sub-bullet indented under the task (2 spaces + `- depends: task-1, task-3`).
4. No nesting deeper than one sub-level. Blockquotes, code fences, and regular paragraphs are ignored.
5. IDs are assigned sequentially (`task-1`, `task-2`, …) in the order tasks appear.
6. Save the plan file with `fs.write`. After saving, use `todo_read` to verify the parsed result and `todo_write` / `todo_cross_off` / `todo_edit` to update tasks during execution.
7. When all tasks in a group are completed, use `VerifyGroup` to confirm tests were run before moving to the next group.

Example:

## Database
- [ ] Design schema
- [ ] Write migrations

## API
- [ ] User endpoints
  - depends: task-1
- [x] Health check
";

/// Plan instructs the model on how to write plans and provides
/// stateful wrappers over the pure todo operations.
#[derive(Default)]
pub struct Plan {
    list: TodoList,
    with_test: bool,
}

impl Plan {
    /// Creates an empty plan.
    #[must_use]
    pub fn new() -> Self {
        Plan {
            list: TodoList::default(),
            with_test: false,
        }
    }

    /// Returns a reference to the internal todo list.
    #[must_use]
    pub fn list(&self) -> &TodoList {
        &self.list
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

    /// Parse a Markdown plan file and load it as the internal state.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the file cannot be read or parsed.
    pub fn load_from_md(&mut self, path: &str) -> Result<(), PlanError> {
        self.list = plan_from_md(path)?;
        Ok(())
    }

    /// Set whether test generation is expected after each completed group.
    /// Default is `false` (no test expectation).
    pub fn with_test(&mut self, value: bool) -> &mut Self {
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
