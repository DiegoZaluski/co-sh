//! Demonstrate `plan`: the stateful TODO list. Builds a plan in a single
//! full-state `todo_write` call (with key-based dependencies), rewrites it
//! to move task statuses (the Claude-Code-style contract: status changes are
//! just writing the list again), exercises the one-in-progress rule,
//! dependency nags, and clearing with an empty list.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example plan
//! ```

use cosh_tools::plan::{
    Plan, TodoItemInput, TodoList, TodoStatus, TodoWriteInput, todo_write,
};

/// Render a list the way the harness's protected context block does:
/// checkbox markers and dependencies.
fn render(list: &TodoList) -> String {
    let mut out = String::new();
    for item in &list.items {
        let marker = match item.status {
            TodoStatus::Pending => ' ',
            TodoStatus::InProgress => '*',
            TodoStatus::Completed => 'x',
            TodoStatus::Cancelled => '-',
        };
        let mut line = format!("- [{}] {} ({})", marker, item.description, item.id);
        if !item.depends_on.is_empty() {
            line.push_str(&format!("  (depends: {})", item.depends_on.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn show(label: &str, plan: &Plan) {
    println!("== {label} ==");
    print!("{}", render(plan.list()));
    println!();
}

fn show_nags(label: &str, nags: &[cosh_tools::plan::types::Nag]) {
    println!("== {label} ==");
    for nag in nags {
        println!("  ! {}\n", nag.message);
    }
    println!();
}

fn main() {
    // ── 1. Build the plan in a single full-state write --------------------
    let mut plan = Plan::new();
    let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [
            { "description": "Design schema", "key": "schema" },
            { "description": "Write migrations", "key": "migrations", "depends_on": ["schema"] },
            { "description": "Health check" }
        ]
    }))
    .unwrap();
    let out = plan.todo_write(&input.todos).unwrap();
    show_nags("1. full write (whole plan in one call, keys resolved)", &out.nags);
    show("   ...ids assigned in listed order", &plan);

    // ── 2. Dependencies are advisory: a missing dep nags, not errors ------
    let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [
            { "description": "Design schema", "key": "schema" },
            { "description": "Write migrations", "key": "migrations", "depends_on": ["schema"] },
            { "description": "Health check", "depends_on": ["task-99"] }
        ]
    }))
    .unwrap();
    let out = plan.todo_write(&input.todos).unwrap();
    show_nags("2. write with a nonexistent dependency", &out.nags);

    // ── 3. Status changes are rewrites, and the one-in-progress rule ------
    let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [
            { "description": "Design schema", "status": "in_progress" },
            { "description": "Write migrations" },
            { "description": "Health check" }
        ]
    }))
    .unwrap();
    plan.todo_write(&input.todos).unwrap();
    show("3. after starting task-1 (rewrite with in_progress)", &plan);
    let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [
            { "description": "Design schema", "status": "in_progress" },
            { "description": "Write migrations", "status": "in_progress" },
            { "description": "Health check" }
        ]
    }))
    .unwrap();
    match plan.todo_write(&input.todos) {
        Ok(_) => panic!("two in_progress tasks must be rejected"),
        Err(e) => {
            println!("== 3b. two in_progress rejected ==");
            println!("  ! {}\n", e.0);
        }
    }

    // ── 4. Complete by rewriting with the new status -----------------------
    let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
        "todos": [
            { "description": "Design schema", "status": "completed" },
            { "description": "Write migrations" },
            { "description": "Health check" }
        ]
    }))
    .unwrap();
    plan.todo_write(&input.todos).unwrap();
    show("4. after completing task-1", &plan);

    // ── 5. Clear with an empty list ----------------------------------------
    let out = plan.todo_write(&[]).unwrap();
    show_nags("5. empty write clears the plan", &out.nags);
    show("   ...after clear", &plan);

    // ── 6. The pure free-function form -------------------------------------
    let out = todo_write(&[TodoItemInput {
        key: None,
        description: "pure function".into(),
        status: TodoStatus::Pending,
        depends_on: None,
    }])
    .unwrap();
    println!("== 6. free-function todo_write ==");
    print!("{}", render(&out.list));
}
