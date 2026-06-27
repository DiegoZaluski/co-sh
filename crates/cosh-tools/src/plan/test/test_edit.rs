use crate::plan::{TodoEdit, TodoList, todo_edit};

use super::helpers::add_task;

#[test]
fn update_description() {
    let list = add_task(&TodoList::default(), "default", "Old");
    let output = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: Some("New".into()),
            group: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.groups[0].items[0].description, "New");
}

#[test]
fn update_moves_group() {
    let list = add_task(&TodoList::default(), "a", "Task");
    let output = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: None,
            group: Some("b".into()),
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.groups.len(), 2);
    assert!(output.list.groups[0].items.is_empty());
    assert_eq!(output.list.groups[1].items.len(), 1);
    assert_eq!(output.list.groups[1].items[0].id, "task-1");
}

#[test]
fn update_empty_description_rejected() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let result = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: Some("".into()),
            group: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn update_depends_on() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let output = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: None,
            group: None,
            depends_on: Some(vec!["other".into()]),
        },
    )
    .unwrap();
    assert_eq!(output.list.groups[0].items[0].depends_on, vec!["other"]);
}

#[test]
fn update_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_edit(
        &list,
        &TodoEdit {
            id: "ghost".into(),
            description: None,
            group: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn update_depends_on_self_reference_nags() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let output = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: None,
            group: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("self-reference"));
    assert!(has_nag);
}

#[test]
fn update_depends_on_nonexistent_nags() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let output = todo_edit(
        &list,
        &TodoEdit {
            id: "task-1".into(),
            description: None,
            group: None,
            depends_on: Some(vec!["ghost".into()]),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("does not exist"));
    assert!(has_nag);
}
