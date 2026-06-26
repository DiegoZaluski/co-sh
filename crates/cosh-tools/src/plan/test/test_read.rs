use crate::plan::{TodoList, TodoReadAction, TodoStatus, todo_read};

use super::helpers::populated_list;

#[test]
fn list_all() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: None,
            status: None,
        },
    )
    .unwrap();
    assert_eq!(output.groups.len(), 2);
    assert_eq!(output.groups[0].items.len(), 2);
    assert_eq!(output.groups[1].items.len(), 1);
}

#[test]
fn list_filter_by_group() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: Some("frontend".into()),
            status: None,
        },
    )
    .unwrap();
    assert_eq!(output.groups.len(), 1);
    assert_eq!(output.groups[0].title, "frontend");
}

#[test]
fn list_filter_by_status() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: None,
            status: Some(TodoStatus::Completed),
        },
    )
    .unwrap();
    let total: usize = output.groups.iter().map(|g| g.items.len()).sum();
    assert_eq!(total, 1);
}

#[test]
fn list_filter_by_group_and_status() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: Some("backend".into()),
            status: Some(TodoStatus::InProgress),
        },
    )
    .unwrap();
    assert_eq!(output.groups[0].items.len(), 1);
    assert_eq!(output.groups[0].items[0].id, "task-1");
}

#[test]
fn list_empty_when_no_match() {
    let list = populated_list();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: None,
            status: Some(TodoStatus::Cancelled),
        },
    )
    .unwrap();
    let total: usize = output.groups.iter().map(|g| g.items.len()).sum();
    assert_eq!(total, 0);
}

#[test]
fn list_empty_list() {
    let list = TodoList::default();
    let output = todo_read(
        &list,
        &TodoReadAction::List {
            group: None,
            status: None,
        },
    )
    .unwrap();
    assert!(output.groups.is_empty());
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
    assert_eq!(output.groups.len(), 1);
    assert_eq!(output.groups[0].title, "backend");
    assert_eq!(output.groups[0].items.len(), 1);
    assert_eq!(output.groups[0].items[0].description, "Task B");
    assert_eq!(
        output.groups[0].items[0].status,
        TodoStatus::Completed
    );
}

#[test]
fn get_nonexistent_fails() {
    let list = populated_list();
    let result = todo_read(
        &list,
        &TodoReadAction::Get {
            id: "ghost".into(),
        },
    );
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
