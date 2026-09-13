# `todo_write` — the full-state write

`todo_write` is the only operation: it replaces the entire TODO list with the
submitted one. The submitted list IS the new state — to change any task's
status, rewrite the whole list with the updated status (the dominant trained
pattern, e.g. Claude Code's `TodoWrite`).

```rust,ignore
pub fn todo_write(todos: &[TodoItemInput])
    -> Result<TodoWriteOutput, PlanError>
```

An `Err` means the write was rejected and **nothing changed**. A success
means the list was replaced — but read the `nags`, because some problems are
tolerated rather than rejected (see [nags are advice, errors are
rejections](plan.md#1-nags-are-advice-errors-are-rejections)).

The [`Plan`](plan.md#the-plan-wrapper) wrapper method
`Plan::todo_write(&mut self, todos)` applies the returned list to its
internal state automatically.

## The submitted shape

```rust,ignore
let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
    "todos": [
        { "description": "Design schema", "key": "schema" },
        { "description": "Write migrations", "depends_on": ["schema"] },
        { "description": "Health check", "status": "pending" }
    ]
}))?;
```

Each entry carries:

| Field | Meaning |
|---|---|
| `description` | Required, never empty. |
| `status` | `pending` (default when omitted), `in_progress`, `completed`, or `cancelled`. |
| `key` | Optional alias other tasks **in the same call** reference in `depends_on`; resolved to the assigned `task-N` id. Must not look like a task id (`task-<number>`) — that would shadow the ids assigned by the same call. |
| `depends_on` | Optional list of sibling `key`s or existing `task-N` ids. |

## What the write does

- The previous list is discarded; the replacement gets `task-1..N` ids in
  listed order.
- An **empty list clears the plan** (the protected context block disappears).
- The model sends ALL tasks on every call, including unchanged ones. There is
  no separate start/complete/cancel/edit/remove operation: those are all
  rewrites of the list.

**Errors:**

- An empty or whitespace-only `description` is rejected
  (`"Description must be non-empty text (item N)."`);
- Duplicate `key`s are rejected (`"Duplicate key 'x'; keys must be unique."`);
- Keys shaped like `task-<number>` are rejected
  (they would silently shadow the ids assigned by the same call);
- More than one `in_progress` task is rejected
  (`"Only one task at a time can be in_progress; got 'task-1', 'task-2'."`).

**Nags** (the list is still written):

- Unknown dependency references (neither a sibling key nor an existing id);
- Self-references and circular dependency chains;
- An `in_progress` task whose dependencies are not `completed`.

## Example

```rust,ignore
use cosh_tools::plan::{TodoItemInput, TodoStatus, todo_write};

// Create the plan.
let out = todo_write(&[
    TodoItemInput { key: Some("schema".into()), description: "Design schema".into(),
                    status: TodoStatus::Pending, depends_on: None },
    TodoItemInput { key: None, description: "Write migrations".into(),
                    status: TodoStatus::Pending, depends_on: Some(vec!["schema".into()]) },
])?;

// Start the first task: rewrite the full list with the updated status.
let out = todo_write(&[
    TodoItemInput { key: Some("schema".into()), description: "Design schema".into(),
                    status: TodoStatus::InProgress, depends_on: None },
    TodoItemInput { key: None, description: "Write migrations".into(),
                    status: TodoStatus::Pending, depends_on: Some(vec!["schema".into()]) },
])?;

// Two in_progress tasks are rejected.
assert!(todo_write(&[
    TodoItemInput { key: None, description: "A".into(),
                    status: TodoStatus::InProgress, depends_on: None },
    TodoItemInput { key: None, description: "B".into(),
                    status: TodoStatus::InProgress, depends_on: None },
]).is_err());

// Clear the plan.
let out = todo_write(&[])?;
assert!(out.list.items.is_empty());
```

Note the free-function pattern: each call produces a new list and you keep
the returned `list`. The [`Plan`](plan.md#the-plan-wrapper) wrapper does this
bookkeeping for you.
