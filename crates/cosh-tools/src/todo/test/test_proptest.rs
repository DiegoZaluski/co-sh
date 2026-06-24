use proptest::prelude::*;

use crate::todo::{TodoAction, TodoList, TodoStatus, todowrite};

proptest! {
    #[test]
    fn invariant_ids_are_unique_after_any_valid_sequence(
        descs in proptest::collection::vec("[a-zA-Z0-9 _,-]+", 1..10),
    ) {
        let mut list = TodoList::default();
        for desc in &descs {
            if let Ok(output) = todowrite(&list, &TodoAction::Add {
                description: desc.clone(),
                tags: None,
                timeline_ms: None,
                depends_on: None,
            }) {
                list = output.list;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for item in &list.items {
            prop_assert!(seen.insert(&item.id), "duplicate ID: {}", item.id);
        }
    }

    #[test]
    fn invariant_no_task_has_two_in_progress(
        descs in proptest::collection::vec("[a-z]+", 1..6),
        start_idx in any::<usize>(),
    ) {
        let mut list = TodoList::default();
        for desc in &descs {
            let _ = todowrite(&list, &TodoAction::Add {
                description: desc.clone(),
                tags: None,
                timeline_ms: None,
                depends_on: None,
            });
        }

        if list.items.is_empty() {
            return Ok(());
        }

        let idx = start_idx % list.items.len();
        let id = list.items[idx].id.clone();

        if let Ok(output) = todowrite(&list, &TodoAction::Start { id: id.clone() }) {
            list = output.list;
            let in_progress = list.items.iter().filter(|i| i.status == TodoStatus::InProgress).count();
            prop_assert!(
                in_progress <= 1,
                "starting '{}': got {} in-progress tasks",
                id, in_progress
            );

            for item in &list.items {
                if item.id != id && item.status == TodoStatus::Pending {
                    let result = todowrite(&list, &TodoAction::Start { id: item.id.clone() });
                    prop_assert!(result.is_err(), "should not start '{}' while '{}' is in progress", item.id, id);
                    return Ok(());
                }
            }
        }
    }

    #[test]
    fn invariant_status_transition_start(
        start_status in prop_oneof![
            Just(TodoStatus::Pending),
            Just(TodoStatus::InProgress),
            Just(TodoStatus::Completed),
            Just(TodoStatus::Cancelled),
        ],
    ) {
        let items = vec![crate::todo::TodoItem {
            id: "task-1".into(),
            description: "Test".into(),
            status: start_status.clone(),
            tags: crate::todo::TodoTags::default(),
            timeline_ms: None,
            depends_on: vec![],
            rationale: None,
            resolution_tags: vec![],
            created_at: 0,
            updated_at: 0,
        }];
        let list = TodoList { items };

        let result = todowrite(&list, &TodoAction::Start { id: "task-1".into() });
        match start_status {
            TodoStatus::Completed => prop_assert!(result.is_ok(), "should allow re-starting completed"),
            TodoStatus::Cancelled => prop_assert!(result.is_ok(), "should allow re-starting cancelled"),
            _ => {}
        }
    }
}

#[test]
fn deterministic_full_lifecycle() {
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

    let list = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    assert_eq!(list.items[0].status, TodoStatus::InProgress);

    assert!(
        todowrite(
            &list,
            &TodoAction::Start {
                id: "task-2".into()
            }
        )
        .is_err()
    );

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
    assert_eq!(list.items[0].status, TodoStatus::Completed);

    let list = todowrite(
        &list,
        &TodoAction::Start {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    assert_eq!(list.items[1].status, TodoStatus::InProgress);

    let list = todowrite(
        &list,
        &TodoAction::Cancel {
            id: "task-2".into(),
        },
    )
    .unwrap()
    .list;
    assert_eq!(list.items[1].status, TodoStatus::Cancelled);

    assert!(
        !list
            .items
            .iter()
            .any(|i| i.status == TodoStatus::InProgress),
        "no task should be InProgress after cancelling"
    );
}

#[test]
fn clean_preserves_in_progress() {
    let list = TodoList::default();

    let list = todowrite(
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
        &TodoAction::Start {
            id: "task-1".into(),
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

    let output = todowrite(&list, &TodoAction::Clean { keep_pending: true }).unwrap();
    assert_eq!(output.list.items.len(), 1);
    assert_eq!(output.list.items[0].id, "task-1");
    assert_eq!(output.list.items[0].status, TodoStatus::InProgress);
}

#[test]
fn add_remove_readd_ids_are_unique_within_list() {
    let list = TodoList::default();

    let list = todowrite(
        &list,
        &TodoAction::Add {
            description: "First".into(),
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
            description: "Second".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap()
    .list;

    // Remove first, keep second
    let list = todowrite(
        &list,
        &TodoAction::Remove {
            id: "task-1".into(),
        },
    )
    .unwrap()
    .list;
    assert_eq!(list.items.len(), 1);

    // Add a new task — must not collide with remaining task-2
    let output = todowrite(
        &list,
        &TodoAction::Add {
            description: "Third".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();

    let ids: Vec<&str> = output.list.items.iter().map(|i| i.id.as_str()).collect();
    let mut unique = std::collections::HashSet::new();
    assert!(
        ids.iter().all(|id| unique.insert(*id)),
        "IDs must be unique: {:?}",
        ids
    );
}

#[test]
fn nags_are_always_warning() {
    let list = TodoList::default();
    let output = todowrite(
        &list,
        &TodoAction::Add {
            description: "Nag test".into(),
            tags: None,
            timeline_ms: None,
            depends_on: None,
        },
    )
    .unwrap();
    for nag in &output.nags {
        assert_eq!(nag.severity, crate::todo::NagSeverity::Warning);
    }
}
