# `plan` data types: the TODO data model

The plan module's types fall into three groups: the state itself
(`TodoList` → `TaskGroup` → `TodoItem`), the request/response types for the
five tools, and the error type. All state types implement `Serialize`, so the
harness can render the list to JSON (and mirror it into the context block);
the action/input types additionally derive `Deserialize` + `JsonSchema` and
double as the tool argument schemas.

---

## The state model

### `TodoStatus`

```rust
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
```

The four states a task can be in. The first two are *active*, the last two
are *terminal*. Terminal states are sticky: `todo_cross_off` refuses to
change a task that is already `Completed` or `Cancelled`.

### `TodoItem`

```rust
pub struct TodoItem {
    pub id: String,             // "task-1", "task-2", … assigned sequentially
    pub description: String,    // free text, never empty
    pub status: TodoStatus,
    pub depends_on: Vec<String>, // ids of tasks this one depends on
}
```

IDs are assigned by the list, not the caller: every `Add` gets the next
`task-<N>` in sequence — the highest existing number + 1, across all groups —
so ids never collide within a live list. (Removing the highest-numbered task
can cause its number to be reused by a later `Add`.) `depends_on` stores the
ids of prerequisite tasks.

### `TaskGroup`

```rust
pub struct TaskGroup {
    pub title: String,
    pub items: Vec<TodoItem>,
    pub tests_verified: bool,   // set via VerifyGroup after tests pass
}
```

A named bucket of tasks. Groups are created implicitly the first time an
`Add` (or an `Edit` that moves a task) targets a new title. `tests_verified`
is the group-level flag behind the [verification
contract](plan.md#2-the-verification-contract).

### `TodoList`

```rust
#[derive(Default)]
pub struct TodoList {
    pub groups: Vec<TaskGroup>,
}
```

The whole state: an ordered list of groups. This is what `Plan` owns, what
every free function takes as input, and what every output embeds.

---

## Request types

Each tool's input is a thin wrapper struct (`TodoReadInput`, `TodoWriteInput`,
`TodoEditInput`, `TodoCrossOffInput`) holding the
payload below. They exist so the harness can deserialize the tool call
arguments straight into typed values.

### `TodoReadAction` — the `plan_todo_read` payload

```rust
#[serde(tag = "type")]
pub enum TodoReadAction {
    List { group: Option<String>, status: Option<TodoStatus> },
    Get  { id: String },
}
```

`List` returns all groups, optionally filtered by group title and/or task
status. `Get` returns a single group containing the one task.

### `TodoWriteAction` — the `plan_todo_write` payload

```rust
#[serde(tag = "type")]
pub enum TodoWriteAction {
    Add { group: String, description: String, depends_on: Option<Vec<String>> },
    Start { id: String },
    Remove { id: String },
    Clean { keep_pending: bool },
    VerifyGroup { group: String },
}
```

Each variant is one mutation; see [todo_write](todo_write.md) for the full
semantics of each.

### `TodoEdit` and `TodoCrossOff`

```rust
pub struct TodoEdit {
    pub id: String,
    pub description: Option<String>,  // None = leave unchanged
    pub group: Option<String>,        // None = leave unchanged
    pub depends_on: Option<Vec<String>>,
}

#[serde(tag = "type")]
pub enum TodoCrossOff {
    Complete { id: String },
    Cancel { id: String },
}
```

`TodoEdit` is deliberately *partial*: only the fields you provide are
updated. `TodoCrossOff` picks the terminal state to move a task into.

---

## Output types

### `TodoWriteOutput` — mutations

```rust
pub struct TodoWriteOutput {
    pub list: TodoList,     // the FULL new state after the mutation
    pub nags: Vec<Nag>,
}
```

Every mutation (`todo_write`, `todo_edit`, `todo_cross_off`) returns the
whole new list plus any advisory nags. The `Plan` wrapper replaces its
internal state with `output.list` automatically; callers of the free
functions must do that themselves.

### `TodoReadOutput` — reads

```rust
pub struct TodoReadOutput {
    pub groups: Vec<TaskGroup>,  // the matching groups (filtered by the action)
    pub nags: Vec<Nag>,
}
```

For `Get`, `groups` holds exactly one group with one item.

### `Nag`

```rust
pub struct Nag {
    pub message: String,
}
```

An advisory message the caller should read and act on. Nags never prevent an
operation from succeeding — see [nags are advice, errors are
rejections](plan.md#1-nags-are-advice-errors-are-rejections) on the module
page.

---

## `PlanError`

```rust
#[derive(thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);
```

The single error type for the whole module. It wraps a human-readable
message that doubles as a correction prompt for the model (e.g. `"Task
'ghost' does not exist. Use List to see available tasks."`). An `Err` means
the operation was rejected and the list was left unchanged.
