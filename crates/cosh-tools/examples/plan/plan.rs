//! Demonstrate `plan`: the stateful TODO list. Builds a plan with `Add`
//! (including dependency nags), exercises `Start` (one-in-progress rule),
//! `Edit`, `CrossOff` with dependent warnings, filtered reads, the
//! VerifyGroup verification contract, `Clean`, the pure free-function form,
//! and the `load_from_md` Markdown round-trip.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example plan
//! ```

use cosh_tools::plan::{
    Plan, TodoCrossOff, TodoEdit, TodoList, TodoReadAction, TodoStatus, TodoWriteAction,
    todo_read, todo_write,
};

/// Render a list the way the harness's protected context block does:
/// groups, checkbox markers, and dependencies.
fn render(list: &TodoList) -> String {
    let mut out = String::new();
    for group in &list.groups {
        out.push_str(&format!("\n### {}\n", group.title));
        for item in &group.items {
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
        if group.tests_verified {
            out.push_str("  (tests verified)\n");
        }
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
    // ── 1. Build the plan ------------------------------------------------
    let mut plan = Plan::new();
    plan.todo_write(&TodoWriteAction::Add {
        group: "Database".into(),
        description: "Design schema".into(),
        depends_on: None,
    })
    .unwrap();
    plan.todo_write(&TodoWriteAction::Add {
        group: "Database".into(),
        description: "Write migrations".into(),
        depends_on: Some(vec!["task-1".into()]),
    })
    .unwrap();
    plan.todo_write(&TodoWriteAction::Add {
        group: "API".into(),
        description: "Health check".into(),
        depends_on: None,
    })
    .unwrap();
    show("1. after three Adds (ids assigned sequentially)", &plan);

    // ── 2. Dependencies are advisory: a missing dep nags, not errors ------
    let out = plan
        .todo_write(&TodoWriteAction::Add {
            group: "API".into(),
            description: "Auth".into(),
            depends_on: Some(vec!["task-99".into()]),
        })
        .unwrap();
    show_nags("2. Add with a nonexistent dependency", &out.nags);

    // ── 3. Start, and the one-in-progress rule ----------------------------
    plan.todo_write(&TodoWriteAction::Start { id: "task-1".into() })
        .unwrap();
    match plan.todo_write(&TodoWriteAction::Start { id: "task-2".into() }) {
        Ok(_) => panic!("second Start must be rejected"),
        Err(e) => {
            println!("== 3. second Start rejected ==");
            println!("  ! {}\n", e.0);
        }
    }
    show("   ...after Start task-1", &plan);

    // ── 4. Edit: rename and move a task in one partial edit ---------------
    plan.todo_edit(&TodoEdit {
        id: "task-1".into(),
        description: Some("Design the schema".into()),
        group: Some("Backend".into()),
        depends_on: None,
    })
    .unwrap();
    show("4. after edit (rename + move to Backend)", &plan);

    // ── 5. Cross off: terminal states are sticky, dependents are warned ----
    let out = plan
        .todo_cross_off(&TodoCrossOff::Complete { id: "task-1".into() })
        .unwrap();
    show_nags("5. completing task-1 (task-2 depends on it)", &out.nags);
    match plan.todo_cross_off(&TodoCrossOff::Complete { id: "task-1".into() }) {
        Ok(_) => panic!("crossing off a terminal task must fail"),
        Err(e) => {
            println!("== 5b. second Complete rejected (sticky) ==");
            println!("  ! {}\n", e.0);
        }
    }

    // ── 6. Filtered reads --------------------------------------------------
    let out = plan
        .todo_read(&TodoReadAction::List {
            group: Some("Database".into()),
            status: Some(TodoStatus::Pending),
        })
        .unwrap();
    println!("== 6. List (group=Database, status=Pending) ==");
    print!("{}", render(&TodoList { groups: out.groups }));
    println!();

    // ── 7. The verification contract --------------------------------------
    plan.todo_cross_off(&TodoCrossOff::Complete { id: "task-2".into() })
        .unwrap();
    let out = plan
        .todo_read(&TodoReadAction::List {
            group: None,
            status: None,
        })
        .unwrap();
    // task-1 lives in Backend now, so both Database and Backend nag.
    show_nags("7. read after Database + Backend are fully terminal", &out.nags);
    plan.todo_write(&TodoWriteAction::VerifyGroup {
        group: "Database".into(),
    })
    .unwrap();
    plan.todo_write(&TodoWriteAction::VerifyGroup {
        group: "Backend".into(),
    })
    .unwrap();
    let out = plan
        .todo_read(&TodoReadAction::List {
            group: None,
            status: None,
        })
        .unwrap();
    assert!(
        out.nags.is_empty(),
        "verifying the groups must clear the verification nags"
    );
    show("   ...after VerifyGroup on both, no more nags", &plan);

    // ── 8. Clean -----------------------------------------------------------
    let out = plan
        .todo_write(&TodoWriteAction::Clean { keep_pending: true })
        .unwrap();
    show_nags("8. Clean(keep_pending: true)", &out.nags);
    show("   ...after clean", &plan);

    // ── 9. The pure free-function form -------------------------------------
    let mut list = TodoList::default();
    let out = todo_write(
        &list,
        &TodoWriteAction::Add {
            group: "Free".into(),
            description: "pure function".into(),
            depends_on: None,
        },
    )
    .unwrap();
    list = out.list; // the free functions return the new list; you thread it
    let out = todo_read(
        &list,
        &TodoReadAction::List {
            group: None,
            status: None,
        },
    )
    .unwrap();
    println!("== 9. free functions (no Plan state) ==");
    println!("  groups: {}, nags: {}\n", out.groups.len(), out.nags.len());

    // ── 10. load_from_md: parse a Markdown plan file -----------------------
    let path = std::env::temp_dir().join("cosh-plan-example.md");
    std::fs::write(
        &path,
        "# Plan: Ship the API\n\n\
         ## Database\n\
         - [ ] Design schema\n\
         - [x] Write migrations\n\
           - depends: task-1\n\n\
         ## API\n\
         - [ ] Health check\n",
    )
    .unwrap();
    plan.load_from_md(&path.to_string_lossy()).unwrap();
    std::fs::remove_file(&path).ok();
    show("10. after load_from_md (parsed file, ids renumbered)", &plan);
    let out = plan
        .todo_read(&TodoReadAction::Get { id: "task-2".into() })
        .unwrap();
    println!("== 10b. Get task-2 ==");
    print!("{}", render(&TodoList { groups: out.groups }));
    println!();
}
