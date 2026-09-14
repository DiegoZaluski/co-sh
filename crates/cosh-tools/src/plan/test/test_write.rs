use crate::plan::{Plan, TodoItemInput, TodoList, TodoStatus, todo_write};

use super::helpers::input;

fn with_deps(description: &str, depends_on: Vec<String>) -> TodoItemInput {
    TodoItemInput {
        key: None,
        description: description.into(),
        status: TodoStatus::Pending,
        depends_on: Some(depends_on),
    }
}

fn keyed(key: &str, description: &str) -> TodoItemInput {
    TodoItemInput {
        key: Some(key.into()),
        description: description.into(),
        status: TodoStatus::Pending,
        depends_on: None,
    }
}

// Full-state write

#[test]
fn full_write_produces_the_submitted_list() {
    let output = todo_write(&[input("New A"), input("New B")]).unwrap();
    assert_eq!(output.list.items.len(), 2);
    assert_eq!(output.list.items[0].description, "New A");
}

#[test]
fn plan_replaces_its_list_on_every_write() {
    let mut plan = Plan::new();
    plan.todo_write(&[input("Old")]).unwrap();
    plan.todo_write(&[input("New A"), input("New B")]).unwrap();
    assert_eq!(
        plan.list().items.len(),
        2,
        "the previous list is fully replaced"
    );
}

#[test]
fn empty_write_clears_the_list() {
    let output = todo_write(&[]).unwrap();
    assert!(output.list.items.is_empty(), "empty list clears the state");
}

#[test]
fn ids_are_assigned_in_listed_order() {
    let output = todo_write(&[input("A"), input("B"), input("C")]).unwrap();
    assert_eq!(output.list.items[0].id, "task-1");
    assert_eq!(output.list.items[1].id, "task-2");
    assert_eq!(output.list.items[2].id, "task-3");
}

#[test]
fn status_defaults_to_pending_when_omitted() {
    let input: crate::plan::TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [{ "description": "A" }]
    }))
    .unwrap();
    let output = todo_write(&input.todos).unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Pending);
}

#[test]
fn statuses_round_trip_snake_case() {
    let list: TodoList = serde_json::from_value(serde_json::json!({
        "items": [
            { "id": "task-1", "description": "A", "status": "in_progress", "depends_on": [] },
            { "id": "task-2", "description": "B", "status": "cancelled", "depends_on": [] }
        ]
    }))
    .unwrap();
    assert_eq!(list.items[0].status, TodoStatus::InProgress);
    assert_eq!(list.items[1].status, TodoStatus::Cancelled);
    let ser = serde_json::to_value(&list.items[0]).unwrap();
    assert_eq!(ser["status"], "in_progress");
}

#[test]
fn empty_description_rejected() {
    let result = todo_write(&[input("   ")]);
    assert!(result.is_err());
}

#[test]
fn status_changes_are_just_rewrites() {
    // The Claude-Code-style contract: updating a status is writing the full
    // list again. Transition through every status without errors.
    let list = todo_write(&[TodoItemInput {
        status: TodoStatus::InProgress,
        ..input("A")
    }])
    .unwrap()
    .list;
    assert_eq!(list.items[0].status, TodoStatus::InProgress);
    let list = todo_write(&[TodoItemInput {
        status: TodoStatus::Completed,
        ..input("A")
    }])
    .unwrap()
    .list;
    assert_eq!(list.items[0].status, TodoStatus::Completed);
    assert_eq!(list.items[0].description, "A");
    assert_eq!(list.items[0].id, "task-1");
}

#[test]
fn terminal_states_are_not_sticky_across_rewrites() {
    // Full-state writes declare the desired state; resending a completed
    // task as completed is a no-op, and the model may also reopen it.
    let output = todo_write(&[TodoItemInput {
        status: TodoStatus::Completed,
        ..input("A")
    }])
    .unwrap();
    assert_eq!(output.list.items[0].status, TodoStatus::Completed);
    let reopened = todo_write(&[input("A")]).unwrap();
    assert_eq!(reopened.list.items[0].status, TodoStatus::Pending);
}

// One-in-progress rule

#[test]
fn two_in_progress_rejected() {
    let result = todo_write(&[
        TodoItemInput {
            status: TodoStatus::InProgress,
            ..input("A")
        },
        TodoItemInput {
            status: TodoStatus::InProgress,
            ..input("B")
        },
    ]);
    assert!(result.is_err());
}

#[test]
fn completing_the_current_task_frees_the_slot() {
    let output = todo_write(&[
        TodoItemInput {
            status: TodoStatus::Completed,
            ..input("A")
        },
        TodoItemInput {
            status: TodoStatus::InProgress,
            ..input("B")
        },
    ])
    .unwrap();
    assert_eq!(output.list.items[1].status, TodoStatus::InProgress);
}

// Keys and dependencies

#[test]
fn keys_resolve_to_ids() {
    let output = todo_write(&[
        keyed("schema", "Design schema"),
        TodoItemInput {
            depends_on: Some(vec!["schema".into()]),
            ..keyed("migrations", "Write migrations")
        },
    ])
    .unwrap();
    assert_eq!(output.list.items[0].depends_on, Vec::<String>::new());
    assert_eq!(output.list.items[1].depends_on, ["task-1".to_string()]);
}

#[test]
fn existing_task_ids_are_accepted_as_dependencies() {
    let output = todo_write(&[input("A"), with_deps("B", vec!["task-1".into()])]).unwrap();
    assert_eq!(output.list.items[1].depends_on, ["task-1".to_string()]);
}

#[test]
fn duplicate_key_rejected() {
    let result = todo_write(&[keyed("dup", "A"), keyed("dup", "B")]);
    assert!(result.is_err());
}

#[test]
fn key_shadowing_task_id_rejected() {
    let result = todo_write(&[input("A"), keyed("task-1", "B")]);
    assert!(
        result.is_err(),
        "a key shaped like task-N must be rejected (it would shadow the real id)"
    );
}

#[test]
fn unknown_key_nags() {
    let output = todo_write(&[with_deps("A", vec!["ghost-key".into()])]).unwrap();
    let has_nag = output.nags.iter().any(|n| {
        n.message
            .contains("neither a sibling key nor an existing task id")
    });
    assert!(has_nag, "unknown key should nag: {:?}", output.nags);
}

#[test]
fn self_reference_nags() {
    let output = todo_write(&[TodoItemInput {
        depends_on: Some(vec!["self".into()]),
        ..keyed("self", "A")
    }])
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("self-reference"));
    assert!(has_nag, "self-reference should nag: {:?}", output.nags);
}

#[test]
fn cycle_nags() {
    let output = todo_write(&[
        TodoItemInput {
            depends_on: Some(vec!["b".into()]),
            ..keyed("a", "A")
        },
        TodoItemInput {
            depends_on: Some(vec!["a".into()]),
            ..keyed("b", "B")
        },
    ])
    .unwrap();
    let has_cycle_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("circular dependency"));
    assert!(has_cycle_nag, "cycle should nag: {:?}", output.nags);
}

#[test]
fn in_progress_with_unfinished_dependency_nags() {
    let output = todo_write(&[
        input("A"),
        TodoItemInput {
            status: TodoStatus::InProgress,
            depends_on: Some(vec!["task-1".into()]),
            ..input("B")
        },
    ])
    .unwrap();
    let has_nag = output
        .nags
        .iter()
        .any(|n| n.message.contains("which is still"));
    assert!(has_nag, "unfinished dep should nag: {:?}", output.nags);
}

// Tool description

#[test]
fn description_teaches_the_full_state_contract() {
    let description = &Plan::default().description_todo_write;
    let todos = &description["inputSchema"]["properties"]["todos"];
    assert_eq!(todos["type"], "array");
    let text = description["description"].as_str().expect("description");
    assert!(text.contains("replaces the previous list"), "{text}");
    assert!(text.contains("{\"todos\": ["), "teaches by example: {text}");
    assert!(text.contains("in_progress"), "{text}");
    let status = &todos["items"]["properties"]["status"];
    assert_eq!(status["enum"].as_array().unwrap().len(), 4);
}
