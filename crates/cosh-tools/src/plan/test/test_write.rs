use crate::plan::{TodoAction, TodoList, TodoStatus, todo_write};

#[test]
fn add_valid_task() {
    let list = TodoList::default();
    let output = todo_write(
        &list,
        &TodoAction::Add {
            description: "Fix memory leak in parser".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.items.len(), 1);
    assert_eq!(
        output.list.items[0].description,
        "Fix memory leak in parser"
    );
    assert_eq!(output.list.items[0].status, TodoStatus::Pending);
    assert!(output.list.items[0].depends_on.is_empty());
}

#[test]
fn add_with_dependencies() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Prerequisite".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[1].depends_on, vec!["task-1"]);
}

#[test]
fn add_with_nonexistent_dependency_nags() {
    let list = TodoList::default();
    let output = todo_write(
        &list,
        &TodoAction::Add {
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
        &TodoAction::Add {
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
        &TodoAction::Add {
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
        &TodoAction::Add {
            description: "Timed task".into(),
            timeline_ms: Some(60_000),
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].timeline_ms, Some(60_000));
}

#[test]
fn add_generates_incremental_ids() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "First".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Second".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    assert_eq!(list.items[0].id, "task-1");
    assert_eq!(list.items[1].id, "task-2");
}

// Start

#[test]
fn start_task() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task 1".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::InProgress);
}

#[test]
fn start_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoAction::Start { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn start_double_rejected() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task A".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task B".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let list = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;

    let result = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-2".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn start_already_completed_allows_rereopen() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::InProgress);
}

#[test]
fn start_with_satisfied_dependency_no_nag() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Prereq".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-2".into(),
        },
    )
    .unwrap();
    let has_nag = output.nags.iter().any(|n| n.message.contains("depends on"));
    assert!(!has_nag);
}

#[test]
fn start_with_unsatisfied_dependency_nags() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_nag = output.nags.iter().any(|n| n.message.contains("depends on"));
    assert!(has_nag);
}

// Complete

#[test]
fn complete_task() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Simple task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
}

#[test]
fn complete_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoAction::Complete { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn complete_from_pending_without_start() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Quick fix".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
}

// Cancel

#[test]
fn cancel_task() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Wont do".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Cancelled);
}

#[test]
fn cancel_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoAction::Cancel { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn cancel_already_cancelled_is_idempotent() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Cancelled);
}

// Update

#[test]
fn update_description() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Old name".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("New name".into()),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].description, "New name");
}

#[test]
fn update_empty_description_rejected() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todo_write(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("".into()),
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn update_timeline() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            timeline_ms: Some(120_000),
            depends_on: None,
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].timeline_ms, Some(120_000));
}

#[test]
fn update_depends_on() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            timeline_ms: None,
            depends_on: Some(vec!["other".into()]),
        },
    )
    .unwrap();
    assert_eq!(output.list.items[0].depends_on, vec!["other"]);
}

#[test]
fn update_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(
        &list,
        &TodoAction::Update {
            id: "ghost".into(),
            description: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

// Remove

#[test]
fn remove_task() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Remove me".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert!(output.list.items.is_empty());
}

#[test]
fn remove_nonexistent_fails() {
    let list = TodoList::default();
    let result = todo_write(&list, &TodoAction::Remove { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn remove_with_stale_dependency_nags() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Shared dep".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Child A".into(),
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Child B".into(),
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items.len(), 2);
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("still referenced as a dependency"));
    assert!(has_nag, "expected nag about stale dependency references");
}

// Clean

#[test]
fn clean_removes_completed_and_cancelled() {
    let list = TodoList::default();
    let mut list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Active".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Done".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Cancelled".into(),
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
        &TodoAction::Cancel {
            id: "task-3".into(),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(&list, &TodoAction::Clean { keep_pending: true }).unwrap();
    assert_eq!(output.list.items.len(), 1);
    assert_eq!(output.list.items[0].description, "Active");
}

#[test]
fn clean_keep_pending_false_removes_pending_too() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Pending".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "InProgress".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let mut list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Done".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    list = todo_write(
        &list,
        &TodoAction::Complete {
            id: "task-3".into(),
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Clean {
            keep_pending: false,
        },
    )
    .unwrap();
    assert!(output.list.items.is_empty());
}

#[test]
fn clean_empty_list_is_noop() {
    let list = TodoList::default();
    let output = todo_write(&list, &TodoAction::Clean { keep_pending: true }).unwrap();
    assert!(output.list.items.is_empty());
}

// Timeline

#[test]
fn start_with_unset_timeline_no_nag() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "No timeline".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_nag = output.nags.iter().any(|n| n.message.contains("timeline"));
    assert!(!has_nag, "should not nag when no timeline is set");
}

#[test]
fn update_with_timeline_change_triggers_check() {
    let list = TodoList::default();
    let list = todo_write(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todo_write(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            timeline_ms: Some(0),
            depends_on: None,
        },
    )
    .unwrap();
    // May or may not trigger depending on elapsed time — just verify no crash
    assert!(output.list.items[0].timeline_ms == Some(0));
}
