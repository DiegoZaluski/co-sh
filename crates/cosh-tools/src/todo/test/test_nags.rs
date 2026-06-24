use crate::todo::{NagSeverity, TodoAction, TodoList, TodoTags, todowrite};

#[test]
fn test_add_generates_tag_nag() {
    let list = TodoList::default();
    let output = todowrite(
        &list,
        &TodoAction::Add {
            description: "Do something".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();

    let has_tag_nag = output.nags.iter().any(|n| n.message.contains("tags"));
    assert!(has_tag_nag, "Should nag about setting tags");
}

#[test]
fn test_complete_generates_doublecheck_nag() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Do it".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap();

    let has_doublecheck = output
        .nags
        .iter()
        .any(|n| n.message.contains("Double-check"));
    assert!(has_doublecheck);
}

#[test]
fn test_complete_important_nags_for_confirmation() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Critical".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: Some("Fixed the thing".into()),
            resolution_tags: None,
        },
    )
    .unwrap();

    let has_confirmation_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("Ask your user to confirm"));
    assert!(has_confirmation_nag);
}

#[test]
fn test_start_nags_about_tags() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Work".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
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
    let has_tag_nag = output.nags.iter().any(|n| n.message.contains("tags"));
    assert!(has_tag_nag);
}

#[test]
fn test_remove_with_stale_deps_generates_nag() {
    let list = TodoList::default();
    let mut list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Parent".into(),
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
            description: "Child".into(),
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
    let has_stale_nag = output.nags.iter().any(|n| n.message.contains("stale"));
    assert!(has_stale_nag, "Should warn about stale dependency refs");
}

#[test]
fn test_update_bare_task_nags_about_missing_metadata() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Bare".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    // Update with no meaningful metadata change — should still be fine
    // since the task already has possibly_important set.
    let output = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: None,
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    assert!(output.list.items[0].tags.possibly_important);
}

#[test]
fn test_clean_with_removals_nags() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Temp".into(),
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

    let output = todowrite(&list, &TodoAction::Clean { keep_pending: true }).unwrap();
    assert!(output.list.items.is_empty());
    assert!(!output.nags.is_empty(), "Clean should report removed count");
}

#[test]
fn test_add_generates_warning_severity() {
    let list = TodoList::default();
    let output = todowrite(
        &list,
        &TodoAction::Add {
            description: "Test".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();

    for nag in &output.nags {
        assert_eq!(nag.severity, NagSeverity::Warning);
    }
}

#[test]
fn test_start_with_unsatisfied_dependencies_nags() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Dependent task".into(),
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
    let has_dep_nag = output.nags.iter().any(|n| n.message.contains("depends on"));
    assert!(has_dep_nag, "Should nag about unsatisfied dependencies");
}

#[test]
fn test_update_nags_when_metadata_is_bare() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Bare task".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("Updated bare".into()),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    let has_nag = output.nags.iter().any(|n| n.message.contains("no tags"));
    assert!(
        has_nag,
        "should nag when task has no tags, timeline, or deps"
    );
}

#[test]
fn test_update_does_not_nag_when_important() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Important task".into(),
            tags: Some(TodoTags {
                possibly_important: true,
            }),
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    let output = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("Still important".into()),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    let has_metadata_nag = output.nags.iter().any(|n| n.message.contains("no tags"));
    assert!(
        !has_metadata_nag,
        "should NOT nag when task has possibly_important set"
    );
}

#[test]
fn test_update_with_nonexistent_dependency_should_warn() {
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
            depends_on: Some(vec!["nonexistent".into()]),
        },
    )
    .unwrap();

    let has_dep_nag = result
        .nags
        .iter()
        .any(|n| n.message.contains("does not exist"));
    assert!(
        has_dep_nag,
        "should warn when Update sets a non-existent dependency"
    );
}

#[test]
fn test_confirm_resolution_nags_about_persistence() {
    let list = TodoList::default();
    let list = crate::todo::test::test_resolution::make_important_task(&list, "Test");
    let list = crate::todo::test::test_resolution::complete_important(&list, "task-1", "Did it");

    let output = todowrite(
        &list,
        &TodoAction::ConfirmResolution {
            todo_id: "task-1".into(),
            rationale: "Did it well".into(),
            resolution_tags: vec![],
        },
    )
    .unwrap();
    let has_persist_nag = output.nags.iter().any(|n| n.message.contains("persist"));
    assert!(
        has_persist_nag,
        "should remind caller to persist resolution"
    );
}

// Timeline enforcement nags

#[test]
fn test_timeline_nag_when_exceeded_on_start() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Overdue".into(),
            tags: None,
            timeline_ms: Some(1), // expires after 1ms
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    // Ensure at least 1ms elapses so the timeline is definitely exceeded.
    std::thread::sleep(std::time::Duration::from_millis(2));
    let output = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap();
    let has_timeline_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("exceeded its timeline"));
    assert!(has_timeline_nag, "should nag when task exceeds timeline");
}

#[test]
fn test_timeline_nag_when_exceeded_on_complete() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Overdue complete".into(),
            tags: None,
            timeline_ms: Some(1), // expires after 1ms
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    std::thread::sleep(std::time::Duration::from_millis(2));
    let output = todowrite(
        &list,
        &TodoAction::Complete {
            id: "task-1".into(),
            rationale: None,
            resolution_tags: None,
        },
    )
    .unwrap();
    let has_timeline_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("exceeded its timeline"));
    assert!(has_timeline_nag, "should nag when completed past timeline");
}

#[test]
fn test_no_timeline_nag_when_within_budget() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "On time".into(),
            tags: None,
            timeline_ms: Some(u64::MAX), // effectively infinite
            depends_on: None,
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
    let has_timeline_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("exceeded its timeline"));
    assert!(!has_timeline_nag, "should NOT nag when within timeline");
}

#[test]
fn test_self_dependency_should_warn_on_update() {
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
            depends_on: Some(vec!["task-1".into()]),
        },
    )
    .unwrap();

    let has_self_dep_nag = result.nags.iter().any(|n| {
        n.message.contains("self") || n.message.contains("itself") || n.message.contains("own")
    });
    assert!(
        has_self_dep_nag,
        "should warn when a task depends on itself"
    );
}

#[test]
fn test_update_should_nag_when_timeline_exceeded() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "Overdue".into(),
            tags: None,
            timeline_ms: Some(1),
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    std::thread::sleep(std::time::Duration::from_millis(2));

    let output = todowrite(
        &list,
        &TodoAction::Update {
            id: "task-1".into(),
            description: Some("Still overdue".into()),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();

    let has_timeline_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("exceeded its timeline"));
    assert!(
        has_timeline_nag,
        "should nag when Update is called on a task that exceeded its timeline"
    );
}

#[test]
fn test_no_timeline_nag_when_not_set() {
    let list = TodoList::default();
    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "No deadline".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
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
    let has_timeline_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("exceeded its timeline"));
    assert!(!has_timeline_nag, "should NOT nag when no timeline set");
}
