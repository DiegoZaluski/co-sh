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
/// Teaches the model the plan file syntax.
pub const PLAN_WRITE: &str = "";

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
