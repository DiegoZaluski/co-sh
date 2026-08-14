# `todo_cross_off` — completing and cancelling tasks

`todo_cross_off` moves a task into a **terminal** state — `Completed` or
`Cancelled` — in one step:

```rust,ignore
pub fn todo_cross_off(list: &TodoList, action: &TodoCrossOff)
    -> Result<TodoWriteOutput, PlanError>
```

```rust,ignore
use cosh_tools::plan::TodoCrossOff;

// Mark a task done.
todo_cross_off(&list, &TodoCrossOff::Complete { id: "task-1".into() })?;

// Give up on a task that will not happen.
todo_cross_off(&list, &TodoCrossOff::Cancel { id: "task-2".into() })?;
```

The wrapper method is `Plan::todo_cross_off(&mut self, action)`.

---

## Terminal states are sticky

A task may only reach `Completed` or `Cancelled` **once**. Cross-off on a
task that is already in a terminal state is rejected:

- `"Task 'task-1' is already completed."`
- `"Task 'task-1' is already cancelled."`

The same task cannot be cancelled after being completed (or vice versa).
There is no "undo" in the todo system — un-completing a task is not a
supported operation, by design: the terminal states mark decisions, and a
mistake should be corrected by editing the task's description or removing it
with [`todo_write`](todo_write.md#remove--delete-a-task).

Any non-terminal task (`Pending` or `InProgress`) can be crossed off. Note
that crossing off an `InProgress` task also frees the "one task at a time"
slot for a new [`Start`](todo_write.md#start--mark-a-task-in-progress).

---

## Dependents are warned

When a task is crossed off, every *other* task that lists it in
`depends_on` — and is not itself terminal — gets a nag naming the new state:

```
'task-3' depends on 'task-1' which is now Completed.
```

The nag is advisory: the dependent task stays as it is. It is the model's
job to read the nag and decide whether the dependent still makes sense
(perhaps cancel or re-plan it).

---

## Errors

- Nonexistent task id → `"Task 'x' does not exist. Use List to see available
  tasks."`
- Task already terminal → the "already completed/cancelled" errors above.

---

## Example

```rust,ignore
use cosh_tools::plan::{TodoList, TodoWriteAction, TodoCrossOff, todo_write, todo_cross_off};

let mut list = TodoList::default();
list = todo_write(&list, &TodoWriteAction::Add {
    group: "API".into(),
    description: "User endpoints".into(),
    depends_on: None,
})?.list;
list = todo_write(&list, &TodoWriteAction::Add {
    group: "API".into(),
    description: "Health check".into(),
    depends_on: Some(vec!["task-1".into()]),
})?.list;

// Completing task-1 nags about task-2, which depends on it.
let out = todo_cross_off(&list, &TodoCrossOff::Complete { id: "task-1".into() })?;
assert!(out.nags.iter().any(|n| n.message.contains("depends on 'task-1'")));

// Cross-off is one-way: completing task-1 again is an error.
assert!(todo_cross_off(&out.list, &TodoCrossOff::Complete { id: "task-1".into() }).is_err());
```
