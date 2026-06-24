use crate::todo::{TodoAction, TodoList, TodoStatus, TodoTags, todowrite};

// Add

#[test]
fn test_add_valid_task() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Fix memory leak in parser".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items.len(), 1);
    assert_eq!(
        output.list.items[0].description,
        "Fix memory leak in parser"
    );
    assert_eq!(output.list.items[0].status, TodoStatus::Pending);
    assert!(!output.list.items[0].tags.possibly_important);
    assert!(output.list.items[0].depends_on.is_empty());
}

#[test]
fn test_add_with_tags() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Important refactor".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items.len(), 1);
    assert!(output.list.items[0].tags.possibly_important);
}

#[test]
fn test_add_with_dependencies() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Subtask".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items[0].depends_on, vec!["task-1"]);
    let has_self_dep_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("self-reference"));
    assert!(
        has_self_dep_nag,
        "should warn about self-referencing dependency when task depends on its own ID"
    );
}

#[test]
fn test_add_empty_description_rejected() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("empty description"));
}

#[test]
fn test_start_task() {
    let mut list = TodoList::default();
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task 1".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::InProgress);
}

#[test]
fn test_start_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(&list, &TodoAction::Start { id: "ghost".into() });
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_double_start_rejected() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task A".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task B".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let list = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-2".into(),
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("still in progress"));
}

#[test]
fn test_complete_task() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Simple task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
}

#[test]
fn test_complete_important_requires_rationale() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Critical fix".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("rationale"));
}

#[test]
fn test_complete_important_with_rationale_ok() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Critical fix".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: Some("Used Two-Finger algorithm for O(n) perf".into()),
            resolution_tags: Some(vec!["optimization".into()]),
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
    assert_eq!(
        output.list.items[0].rationale.as_deref(),
        Some("Used Two-Finger algorithm for O(n) perf")
    );
    let has_confirmation_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("Ask your user to confirm"));
    assert!(has_confirmation_nag);
}

#[test]
fn test_cancel_task() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Wont do".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].status, TodoStatus::Cancelled);
}

#[test]
fn test_update_task() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Old name".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("New name".into()),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: Some(30_000),
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    let item = &result.unwrap().list.items[0];
    assert_eq!(item.description, "New name");
    assert!(item.tags.possibly_important);
    assert_eq!(item.timeline_ms, Some(30_000));
}

#[test]
fn test_update_empty_description_rejected() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("".into()),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn test_remove_task() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Remove me".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert!(result.unwrap().list.items.is_empty());
}

#[test]
fn test_remove_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(&list, &TodoAction::Remove { id: "ghost".into() });
    assert!(result.is_err());
}

#[test]
fn test_clean_removes_completed() {
    let list = TodoList::default();
    let mut list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Active".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Done".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Cancelled".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    list = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-2".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Cancel {
            id: "task-3".into(),
        },
    )
    .unwrap()
    .list;

    let result = todowrite(&list, &TodoAction::Clean { keep_pending: true });
    assert!(result.is_ok());
    let cleaned = result.unwrap().list;
    assert_eq!(cleaned.items.len(), 1);
    assert_eq!(cleaned.items[0].description, "Active");
}

#[test]
fn test_clean_keep_pending_false_removes_pending_too() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Pending".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "InProgress".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let mut list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Done".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    list = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-3".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Clean {
            keep_pending: false,
        },
    );
    assert!(result.is_ok());
    assert!(result.unwrap().list.items.is_empty());
}

#[test]
fn test_add_with_timeline() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Timed task".into(),
            tags: None,
            timeline_ms: Some(60_000),
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].timeline_ms, Some(60_000));
}

#[test]
fn test_add_with_existing_dependency_no_nag() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Prerequisite".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    );
    assert!(result.is_ok());
    let output = result.unwrap();
    let has_dep_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("does not exist"));
    assert!(!has_dep_nag, "should NOT nag when dependency exists");
}

#[test]
fn test_add_whitespace_only_description_rejected() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "   ".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn test_add_special_characters_description() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Add {
            description: "Fix 🐛 in parser (CVE-2024-1234)".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap().list.items[0].description,
        "Fix 🐛 in parser (CVE-2024-1234)"
    );
}

// Start

#[test]
fn test_start_already_completed_still_works() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
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
        &TodoAction::Start {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].status, TodoStatus::InProgress);
}

#[test]
fn test_start_with_satisfied_dependency_no_nag() {
    let list = TodoList::default();
    let mut list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Prerequisite".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-2".into(),
        },
    )
    .unwrap();
    let has_dep_nag = output.nags.iter().any(|n| n.message.contains("depends on"));
    assert!(!has_dep_nag, "should NOT nag when dependency is satisfied");
}

#[test]
fn test_start_with_stale_dependency_nags() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Dependent".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["prereq".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_stale_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("no longer exists"));
    assert!(has_stale_nag, "should nag about stale dependency");
}

// Complete

#[test]
fn test_complete_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "ghost".into(),
            rationale: None,
            resolution_tags: None,
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_complete_from_pending_without_start() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Quick fix".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].status, TodoStatus::Completed);
}

#[test]
fn test_important_complete_requires_non_whitespace_rationale() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Critical fix".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: Some("   ".into()),
            resolution_tags: None,
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("rationale"));
}

// Cancel

#[test]
fn test_cancel_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(&list, &TodoAction::Cancel { id: "ghost".into() });
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_cancel_already_cancelled_is_idempotent() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
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
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].status, TodoStatus::Cancelled);
}

#[test]
fn test_cancel_completed_task() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
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
        &TodoAction::Cancel {
            id: "task-1".into(),
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].status, TodoStatus::Cancelled);
}

// Update

#[test]
fn test_update_nonexistent_fails() {
    let list = TodoList::default();
    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "ghost".into(),
            description: None,
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_update_partial_fields_only() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            tags: Some(TodoTags {
                possibly_important: false,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    // Only update tags, leave everything else.
    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    let item = &result.unwrap().list.items[0];
    assert_eq!(item.description, "Task");
    assert!(item.tags.possibly_important);
    assert!(item.timeline_ms.is_none());
}

#[test]
fn test_update_timeline_only() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            tags: None,
            timeline_ms: Some(120_000),
            depends_on: None,
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].timeline_ms, Some(120_000));
}

#[test]
fn test_update_depends_on_only() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["other".into()]),
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().list.items[0].depends_on, vec!["other"]);
}

#[test]
fn test_update_bare_task_generates_metadata_nag() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Bare".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    // Update with no meaningful change — triggers metadata nag.
    let output = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("Bare".into()),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    let has_metadata_nag = output.nags.iter().any(|n| n.message.contains("no tags"));
    assert!(has_metadata_nag, "should nag about missing metadata");
}

// Remove

#[test]
fn test_remove_with_multiple_dependents_nags() {
    let list = TodoList::default();
    let mut list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Shared dep".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Child A".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;
    list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Child B".into(),
            tags: None,
            timeline_ms: None,
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap();
    assert_eq!(output.list.items.len(), 2);
    let has_stale_nag = output.nags.iter().any(|n| n.message.contains("stale"));
    assert!(has_stale_nag, "should warn about multiple stale deps");
}

// Clean

#[test]
fn test_clean_empty_list_is_noop() {
    let list = TodoList::default();
    let result = todowrite(&list, &TodoAction::Clean { keep_pending: true });
    assert!(result.is_ok());
    assert!(result.unwrap().list.items.is_empty());
}

#[test]
fn test_clean_keep_pending_true_keeps_pending() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Pending".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Done".into(),
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
            id: "task-2".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap()
    .list;

    let result = todowrite(&list, &TodoAction::Clean { keep_pending: true });
    assert!(result.is_ok());
    let cleaned = result.unwrap().list;
    assert_eq!(cleaned.items.len(), 1);
    assert_eq!(cleaned.items[0].description, "Pending");
}

// ID collision regression demo (will fail until next_id fix)

#[test]
fn test_id_collision_after_remove_and_add() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "A".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "B".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;
    // Remove the first task
    let list = todowrite(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    // Re-add: OLD code generates format!("task-{}", items.len()+1) = "task-1" ← COLLISION
    let output = todowrite(
        &list,
        &TodoAction::Add {
            description: "C".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert_ne!(
        output.list.items[0].id, "task-1",
        "ID collision: re-added task got same ID as removed task"
    );

    // All remaining IDs must be unique
    let ids: Vec<&str> = output.list.items.iter().map(|i| i.id.as_str()).collect();
    let mut unique = std::collections::HashSet::new();
    assert!(
        ids.iter().all(|id| unique.insert(*id)),
        "IDs must be unique after re-add"
    );
}
