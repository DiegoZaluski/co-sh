use crate::todo::{TodoAction, TodoList, TodoStatus, TodoTags, todowrite};

pub(super) fn make_important_task(list: &TodoList, desc: &str) -> TodoList {
    todowrite(
        list,
        &TodoAction::Add {
            description: desc.into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list
}

pub(super) fn complete_important(list: &TodoList, id: &str, rationale: &str) -> TodoList {
    todowrite(
        list,
        &TodoAction::Complete {
            id: id.into(),
            rationale: Some(rationale.into()),
            resolution_tags: Some(vec!["bugfix".into()]),
        },
    )
    .unwrap()
    .list
}

#[test]
fn test_confirm_resolution_after_completion() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Fix OOM in allocator");
    let list = complete_important(&list, "task-1", "Switched to slab allocator");

    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Switched to slab allocator to reduce fragmentation".into(),
            resolution_tags: vec!["bugfix".into(), "performance".into()],
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.pending_resolutions.len(), 1);
    let res = &output.pending_resolutions[0];
    assert_eq!(res.todo_id, "task-1");
    assert_eq!(res.description, "Fix OOM in allocator");
    assert_eq!(
        res.rationale,
        "Switched to slab allocator to reduce fragmentation"
    );
}

#[test]
fn test_confirm_resolution_not_completed_fails() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Not done yet");

    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Some reason".into(),
            resolution_tags: vec![],
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not completed"));
}

#[test]
fn test_confirm_resolution_not_important_fails() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Normal task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Doesn't matter".into(),
            resolution_tags: vec![],
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not tagged"));
}

#[test]
fn test_confirm_resolution_empty_rationale_fails() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Important");
    let list = complete_important(&list, "task-1", "Done it");

    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "".into(),
            resolution_tags: vec![],
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("rationale is empty"));
}

#[test]
fn test_discard_resolution_clears_important_flag() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Was important");
    let list = complete_important(&list, "task-1", "Did thing");

    let result = todowrite(
        &list,
        &TodoAction::DiscardResolution {
            todo_id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert!(!result.unwrap().list.items[0].tags.possibly_important);
}

#[test]
fn test_discard_resolution_not_completed_fails() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Not completed");

    let result = todowrite(
        &list,
        &TodoAction::DiscardResolution {
            todo_id: "task-1".into(),
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not completed"));
}

#[test]
fn test_resolution_lifecycle_full_flow() {
    let list = TodoList::default();

    // 1. Add important task
    let list = make_important_task(&list, "Critical security fix");

    // 2. Complete with rationale
    let list = complete_important(&list, "task-1", "Applied input sanitization");

    // 3. Confirm resolution (simulating human confirmation)
    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Applied input sanitization to prevent SQL injection".into(),
            resolution_tags: vec!["security".into(), "bugfix".into()],
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.pending_resolutions.len(), 1);
    assert_eq!(
        output.pending_resolutions[0].tags,
        vec!["security", "bugfix"]
    );
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
}

#[test]
fn test_confirm_resolution_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "ghost".into(),
            rationale: "reason".into(),
            resolution_tags: vec![],
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_confirm_resolution_cancelled_fails() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Cancelled task");
    let list = todowrite(
        &list,
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Should not work".into(),
            resolution_tags: vec![],
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not completed"));
}

#[test]
fn test_discard_resolution_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::DiscardResolution {
            todo_id: "ghost".into(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn test_clean_with_important_completed_should_warn_about_pending_resolution() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Important done");
    let list = complete_important(&list, "task-1", "Fixed it");

    let result = todowrite(&list, &TodoAction::Clean { keep_pending: true }).unwrap();

    let has_resolution_nag = result
        .nags
        .iter()
        .any(|n| n.message.contains("resolution confirmation"));
    assert!(
        has_resolution_nag,
        "should warn when removing a task that may need resolution confirmation"
    );
}

#[test]
fn test_discard_resolution_already_discarded_is_noop() {
    let list = TodoList::default();
    let list = make_important_task(&list, "Was important");
    let list = complete_important(&list, "task-1", "Did thing");

    // First discard clears the important flag.
    let list = todowrite(
        &list,
        &TodoAction::DiscardResolution {
            todo_id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    assert!(!list.items[0].tags.possibly_important);

    // Second discard should succeed (flag already false, nothing to clear).
    let result = todowrite(
        &list,
        &TodoAction::DiscardResolution {
            todo_id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
}
