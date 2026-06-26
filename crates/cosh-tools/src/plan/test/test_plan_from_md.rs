use std::sync::atomic::{AtomicU32, Ordering};

use crate::plan::{TodoStatus, plan_from_md};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn write_plan(content: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_plan_{n}.md"));
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn parse_empty_file() {
    let path = write_plan("");
    let list = plan_from_md(&path).unwrap();
    assert!(list.groups.is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_single_group_single_task() {
    let path = write_plan("## Backend\n- [ ] Criar API");
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups.len(), 1);
    assert_eq!(list.groups[0].title, "Backend");
    assert_eq!(list.groups[0].items.len(), 1);
    assert_eq!(list.groups[0].items[0].description, "Criar API");
    assert_eq!(list.groups[0].items[0].status, TodoStatus::Pending);
    assert_eq!(list.groups[0].items[0].id, "task-1");
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_completed_task() {
    let path = write_plan("## Backend\n- [x] Testes");
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups[0].items[0].status, TodoStatus::Completed);
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_multiple_groups() {
    let path = write_plan(
        "\
## Backend
- [ ] API

## Frontend
- [ ] Login
- [x] Home",
    );
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups.len(), 2);
    assert_eq!(list.groups[0].title, "Backend");
    assert_eq!(list.groups[0].items.len(), 1);
    assert_eq!(list.groups[1].title, "Frontend");
    assert_eq!(list.groups[1].items.len(), 2);
    assert_eq!(list.groups[1].items[0].id, "task-2");
    assert_eq!(list.groups[1].items[1].id, "task-3");
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_depends_on() {
    let path = write_plan("## Backend\n- [ ] Task\n  - depends: task-1, task-2");
    let list = plan_from_md(&path).unwrap();
    assert_eq!(
        list.groups[0].items[0].depends_on,
        vec!["task-1".to_owned(), "task-2".to_owned()]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_depends_on_single() {
    let path = write_plan("## Backend\n- [ ] Task\n  - depends: task-1");
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups[0].items[0].depends_on, vec!["task-1"]);
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_ignores_unrelated_markdown() {
    let path = write_plan(
        "\
# Plan: Ignored title

Some intro text.

## Backend
- [ ] Task

> blockquote

- regular list item",
    );
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups.len(), 1);
    assert_eq!(list.groups[0].items.len(), 1);
    std::fs::remove_file(&path).ok();
}

#[test]
fn parse_nonexistent_file() {
    let result = plan_from_md("/tmp/nonexistent_plan_test_file.md");
    assert!(result.is_err());
}

#[test]
fn parse_generates_incremental_ids_across_groups() {
    let path = write_plan(
        "\
## A
- [ ] First

## B
- [ ] Second
- [ ] Third",
    );
    let list = plan_from_md(&path).unwrap();
    assert_eq!(list.groups[0].items[0].id, "task-1");
    assert_eq!(list.groups[1].items[0].id, "task-2");
    assert_eq!(list.groups[1].items[1].id, "task-3");
    std::fs::remove_file(&path).ok();
}
