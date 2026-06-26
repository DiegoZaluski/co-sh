use crate::plan::{TodoAction, TodoList, TodoReadAction, TodoStatus, todo_read, todo_write};

fn populated_list() -> TodoList {
    let list = TodoList::default();
    let mut list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task A".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task B".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task C".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    list = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    list
}

#[test]
fn list_all() {
    let list = populated_list();
    let output = todo_read(&list, &TodoReadAction::List { status: None }).unwrap();
    assert_eq!(output.items.len(), 3);
}

#[test]
fn list_filter_by_status() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            status: Some(TodoStatus::Completed),
        },
    )
    .unwrap();
    assert_eq!(output.items.len(), 1);
    assert_eq!(output.items[0].id, "task-2");
}

#[test]
fn list_filter_in_progress() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            status: Some(TodoStatus::InProgress),
        },
    )
    .unwrap();
    assert_eq!(output.items.len(), 1);
    assert_eq!(output.items[0].id, "task-1");
}

#[test]
fn list_empty_when_no_match() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            status: Some(TodoStatus::Cancelled),
        },
    )
    .unwrap();
    assert!(output.items.is_empty());
}

#[test]
fn list_empty_list() {
    let list = TodoList::default();
    let output = todo_read(&list, &TodoReadAction::List { status: None }).unwrap();
    assert!(output.items.is_empty());
}

#[test]
fn get_existing_task() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::Get {
            id: "task-2".into(),
        },
    )
    .unwrap();
    assert_eq!(output.items.len(), 1);
    assert_eq!(output.items[0].description, "Task B");
    assert_eq!(output.items[0].status, TodoStatus::Completed);
}

#[test]
fn get_nonexistent_fails() {
    let list = populated_list();
    let result = todo_read(&list, &TodoReadAction::Get { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn get_from_empty_list_fails() {
    let list = TodoList::default();
    let result = todo_read(
        &list,
        &TodoReadAction::Get {
            id: "task-1".into(),
        },
    );
    assert!(result.is_err());
}
