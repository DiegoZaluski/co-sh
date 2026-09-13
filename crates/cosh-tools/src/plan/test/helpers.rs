use crate::plan::{TodoItemInput, TodoStatus};

pub fn input(description: &str) -> TodoItemInput {
    TodoItemInput {
        key: None,
        description: description.into(),
        status: TodoStatus::Pending,
        depends_on: None,
    }
}
