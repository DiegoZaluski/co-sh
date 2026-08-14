# `todo_read` — querying the list

`todo_read` is the read side of the todo system: list tasks (with optional
filters) or fetch one task by id. It never mutates anything:

```rust,ignore
pub fn todo_read(list: &TodoList, action: &TodoReadAction)
    -> Result<TodoReadOutput, PlanError>
```

```rust,ignore
use cosh_tools::plan::TodoReadAction;

// Everything.
todo_read(&list, &TodoReadAction::List { group: None, status: None })?;

// Only pending tasks in the "Database" group.
todo_read(&list, &TodoReadAction::List {
    group: Some("Database".into()),
    status: Some(TodoStatus::Pending),
})?;

// One task.
todo_read(&list, &TodoReadAction::Get { id: "task-3".into() })?;
```

The wrapper method is `Plan::todo_read(&self, action)` (note: `&self` — reads
do not need a mutable borrow).

---

## `List` — filtering

`List` returns a copy of every group, optionally narrowed:

| Field | Effect |
|---|---|
| `group: Some(title)` | Only the group with that exact title is returned. |
| `status: Some(s)` | Only tasks with that status are returned. |

Filters compose: `group + status` returns one group containing only the
matching tasks. A group that ends up with zero matching items is still
included (as an empty group) — filtering narrows items, it does not hide
groups.

## `Get` — one task

`Get` returns a single `TaskGroup` containing only the requested task, so the
shape of the output is identical to `List` — always `groups[0].items[0]` for
the task. A nonexistent id is the one **error** on this page:
`"Task 'x' not found."`

---

## The verification nags

`List` (and only `List`) attaches nags from the [verification
contract](plan.md#2-the-verification-contract): every group whose tasks are
*all* terminal but whose `tests_verified` flag is still `false` produces

```
Group 'Database' is complete but tests have not been confirmed.
Use VerifyGroup to confirm tests passed.
```

This is the module's gentle enforcement: the list is returned normally, the
nag reminds the model to run tests and call
[`VerifyGroup`](todo_write.md#verifygroup--record-that-tests-passed) before
declaring the group done. Once verified, the nag disappears. `Get` does not
attach these nags (you asked for one task, not a status report).

---

## Example

```rust,ignore
use cosh_tools::plan::{TodoList, TodoStatus, TodoWriteAction, TodoCrossOff, TodoReadAction,
    todo_write, todo_cross_off, todo_read};

let mut list = TodoList::default();
list = todo_write(&list, &TodoWriteAction::Add {
    group: "Backend".into(),
    description: "Design schema".into(),
    depends_on: None,
})?.list;
list = todo_write(&list, &TodoWriteAction::Add {
    group: "Backend".into(),
    description: "Write migrations".into(),
    depends_on: None,
})?.list;

// All tasks are now terminal → List nags about the unverified group.
list = todo_cross_off(&list, &TodoCrossOff::Complete { id: "task-1".into() })?.list;
list = todo_cross_off(&list, &TodoCrossOff::Complete { id: "task-2".into() })?.list;
let out = todo_read(&list, &TodoReadAction::List { group: None, status: None })?;
assert!(out.nags.iter().any(|n| n.message.contains("tests have not been confirmed")));

// Verify the group → the nag is gone.
list = todo_write(&list, &TodoWriteAction::VerifyGroup { group: "Backend".into() })?.list;
let out = todo_read(&list, &TodoReadAction::List { group: None, status: None })?;
assert!(out.nags.is_empty());
```
