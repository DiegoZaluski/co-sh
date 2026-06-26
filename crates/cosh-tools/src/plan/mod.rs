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
    Nag, TaskGroup, TodoItem, TodoList, TodoReadAction, TodoReadOutput, TodoStatus, TodoWriteOutput,
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
6. Use `fs.write` to save the plan to the agreed path. Do NOT attempt to call a dedicated plan tool — just write the file.

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
}

impl Plan {
    /// Creates an empty plan.
    pub fn new() -> Self {
        Plan {
            list: TodoList::default(),
        }
    }

    /// Returns a reference to the internal todo list.
    pub fn list(&self) -> &TodoList {
        &self.list
    }

    pub fn todo_write(&mut self, action: &TodoWriteAction) -> Result<TodoWriteOutput, String> {
        let output = todo_write(&self.list, action)?;
        self.list = output.list.clone();
        Ok(output)
    }

    pub fn todo_edit(&mut self, edit: &TodoEdit) -> Result<TodoWriteOutput, String> {
        let output = todo_edit(&self.list, edit)?;
        self.list = output.list.clone();
        Ok(output)
    }

    pub fn todo_cross_off(&mut self, action: &TodoCrossOff) -> Result<TodoWriteOutput, String> {
        let output = todo_cross_off(&self.list, action)?;
        self.list = output.list.clone();
        Ok(output)
    }

    pub fn todo_read(&self, action: &TodoReadAction) -> Result<TodoReadOutput, String> {
        todo_read(&self.list, action)
    }

    /// Parse a Markdown plan file and load it as the internal state.
    pub fn load_from_md(&mut self, path: &str) -> Result<(), String> {
        self.list = plan_from_md(path)?;
        Ok(())
    }
}
