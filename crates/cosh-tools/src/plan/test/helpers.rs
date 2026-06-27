use crate::plan::{TodoCrossOff, TodoList, TodoWriteAction, todo_cross_off, todo_write};

pub fn add_task(list: &TodoList, group: &str, description: &str) -> TodoList {
    todo_write(
        list,
        &TodoWriteAction::Add {
            group: group.into(),
            description: description.into(),
            depends_on: None,
        },
    )
    .unwrap()
    .list
}

pub fn populated_list() -> TodoList {
    let mut list = TodoList::default();
    list = add_task(&list, "backend", "Task A");
    list = add_task(&list, "backend", "Task B");
    list = add_task(&list, "frontend", "Task C");
    list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    list
}

pub fn add_task_with_deps(
    list: &TodoList,
    group: &str,
    description: &str,
    depends_on: Vec<String>,
) -> TodoList {
    todo_write(
        list,
        &TodoWriteAction::Add {
            group: group.into(),
            description: description.into(),
            depends_on: Some(depends_on),
        },
    )
    .unwrap()
    .list
}
