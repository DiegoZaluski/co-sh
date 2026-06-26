use crate::plan::{TodoCrossOff, TodoList, TodoStatus, TodoWriteAction, todo_cross_off, todo_write};

use super::helpers::{add_task, add_task_with_deps};

// Add

#[test]
fn add_valid_task() {
    let list = TodoList::default();
    let output = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "Fix memory leak".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.groups.len(), 1);
    assert_eq!(output.list.groups[0].title, "default");
    assert_eq!(output.list.groups[0].items.len(), 1);
    assert_eq!(
        output.list.groups[0].items[0].description,
        "Fix memory leak"
    );
    assert_eq!(
        output.list.groups[0].items[0].status,
        TodoStatus::Pending
    );
}

#[test]
fn add_to_existing_group() {
    let list = add_task(&TodoList::default(), "backend", "Task 1");
    let output = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "backend".into(),
            description: "Task 2".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.groups.len(), 1);
    assert_eq!(output.list.groups[0].items.len(), 2);
}

#[test]
fn add_to_different_groups() {
    let list = add_task(&TodoList::default(), "frontend", "UI");
    let output = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "backend".into(),
            description: "API".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.groups.len(), 2);
    assert_eq!(output.list.groups[0].title, "frontend");
    assert_eq!(output.list.groups[1].title, "backend");
}

#[test]
fn add_with_dependencies() {
    let list = add_task(&TodoList::default(), "default", "Prerequisite");
    let output = add_task_with_deps(&list, "default", "Dependent", vec!["task-1".into()]);
    assert_eq!(
        output.groups[0].items[1].depends_on,
        vec!["task-1"]
    );
}

#[test]
fn add_with_nonexistent_dependency_nags() {
    let list = TodoList::default();
    let output = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "Task".into(),
            timeline_ms: None,
            depends_on: Some(vec!["nonexistent".into()]),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("does not exist"));
    assert!(has_nag);
}

#[test]
fn add_empty_description_rejected() {
    let list = TodoList::default();
    let result = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "".into(),
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn add_whitespace_description_rejected() {
    let list = TodoList::default();
    let result = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "   ".into(),
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn add_with_timeline() {
    let list = TodoList::default();
    let output = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "default".into(),
            description: "Timed".into(),
            timeline_ms: Some(60_000),
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(
        output.list.groups[0].items[0].timeline_ms,
        Some(60_000)
    );
}

#[test]
fn add_generates_incremental_ids() {
    let list = add_task(&TodoList::default(), "a", "First");
    let list = add_task(&list, "b", "Second");
    assert_eq!(list.groups[0].items[0].id, "task-1");
    assert_eq!(list.groups[1].items[0].id, "task-2");
}

// Start

#[test]
fn start_task() {
    let list = add_task(&TodoList::default(), "default", "Task 1");
    let output = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        output.list.groups[0].items[0].status,
        TodoStatus::InProgress
    );
}

#[test]
fn start_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoWriteAction::Start { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn start_double_rejected() {
    let list = add_task(&TodoList::default(), "default", "Task A");
    let list = add_task(&list, "default", "Task B");
    let list = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let result = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-2".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn start_in_different_groups_still_rejected() {
    let list = add_task(&TodoList::default(), "a", "Task A");
    let list = add_task(&list, "b", "Task B");
    let list = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let result = todo_write(
        &list,
        &TodoWriteAction::Start {
            id: "task-2".into(),
        },
    );
    assert!(result.is_err());
}

// Remove

#[test]
fn remove_task() {
    let list = add_task(&TodoList::default(), "default", "Remove me");
    let output = todo_write(
        &list,
        &TodoWriteAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert!(output.list.groups[0].items.is_empty());
}

#[test]
fn remove_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoWriteAction::Remove { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn remove_with_stale_dependency_nags() {
    let list = add_task(&TodoList::default(), "default", "Shared dep");
    let list = add_task_with_deps(&list, "default", "Child", vec!["task-1".into()]);
    let output = todo_write(
        &list,
        &TodoWriteAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("still referenced as a dependency"));
    assert!(has_nag);
}

// Clean

#[test]
fn clean_removes_completed_and_cancelled() {
    let mut list = add_task(&TodoList::default(), "default", "Active");
    list = add_task(&list, "default", "Done");
    list = add_task(&list, "default", "Cancelled");
    list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    list = todo_cross_off(
        &list,
        &TodoCrossOff::Cancel {
            id: "task-3".into(),
        },
    )
    .unwrap()
    .list;
    let output = todo_write(&list, &TodoWriteAction::Clean { keep_pending: true }).unwrap();
    assert_eq!(output.list.groups[0].items.len(), 1);
    assert_eq!(output.list.groups[0].items[0].description, "Active");
}

#[test]
fn clean_removes_across_groups() {
    let list = add_task(&TodoList::default(), "a", "A1");
    let list = add_task(&list, "b", "B1");
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    let output = todo_write(&list, &TodoWriteAction::Clean { keep_pending: false }).unwrap();
    assert!(output.list.groups[0].items.is_empty());
    assert!(output.list.groups[1].items.is_empty());
}

#[test]
fn clean_empty_list_is_noop() {
    let list = TodoList::default();
    let output = todo_write(&list, &TodoWriteAction::Clean { keep_pending: true }).unwrap();
    assert!(output.list.groups.is_empty());
}

#[test]
fn clean_nags_count() {
    let list = add_task(&TodoList::default(), "default", "Done");
    let list = todo_cross_off(
        &list,
        &TodoCrossOff::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let output = todo_write(&list, &TodoWriteAction::Clean { keep_pending: false }).unwrap();
    let has_nag = output.nags.iter().any(|n| n.message.contains("Removed 1"));
    assert!(has_nag);
}
