use crate::plan::{TodoCrossOff, TodoList, TodoStatus, todo_cross_off, todo_write, TodoWriteAction};

use super::helpers::add_task;

// Complete

#[test]
fn complete_task() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let output = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        output.list.groups[0].items[0].status,
        TodoStatus::Completed
    );
}

#[test]
fn complete_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "ghost".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn complete_already_completed_fails() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let result = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn complete_already_cancelled_fails() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let result = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn complete_nags_dependents() {
    let list = add_task(&TodoList::default(), "default", "Parent");
    let list = crate::plan::todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "Child".into(),
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;
    let output = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("depends on"));
    assert!(has_nag);
}

// Cancel

#[test]
fn cancel_task() {
    let list = add_task(&TodoList::default(), "default", "Wont do");
    let output = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        output.list.groups[0].items[0].status,
        TodoStatus::Cancelled
    );
}

#[test]
fn cancel_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "ghost".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn cancel_already_cancelled_fails() {
    let list = add_task(&TodoList::default(), "default", "Task");
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let result = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-1".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn cancel_nags_dependents() {
    let list = add_task(&TodoList::default(), "default", "Parent");
    let list = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "Child".into(),
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;
    let output = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("depends on"));
    assert!(has_nag);
}
