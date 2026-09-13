# `plan` data types: the TODO data model

The plan module's types fall into three groups: the state itself
(`TodoList` → `TodoItem`), the request/response types for the tool, and the
error type. All state types implement `Serialize`, so the harness can render
the list to JSON (and mirror it into the context block); the input types
additionally derive `Deserialize` + `JsonSchema` and double as the tool
argument schema.

---

## The state model

### `TodoStatus`

```rust
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
```

The four states a task can be in, serialized as `pending`, `in_progress`,
`completed`, `cancelled`. The first two are *active*, the last two are
*terminal*. Statuses move exclusively through full-state writes: the model
resends the list with the updated status.

### `TodoItem`

```rust
pub struct TodoItem {
    pub id: String,             // "task-1", "task-2", … assigned sequentially
    pub description: String,    // free text, never empty
    pub status: TodoStatus,
    pub depends_on: Vec<String>, // ids of tasks this one depends on
}
```

IDs are assigned by the tool, not the caller: a write assigns `task-1..N` in
listed order. `depends_on` stores the ids of prerequisite tasks.

### `TodoList`

```rust
#[derive(Default)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}
```

The whole state: a flat, ordered list of tasks. This is what `Plan` owns,
what the free function produces, and what every output embeds.

---

## Request types

The tool's input is `TodoWriteInput`:

```rust
pub struct TodoWriteInput {
    pub todos: Vec<TodoItemInput>,
}

pub struct TodoItemInput {
    pub key: Option<String>,        // sibling alias, resolved to task-N
    pub description: String,
    pub status: TodoStatus,         // defaults to `pending` when omitted
    pub depends_on: Option<Vec<String>>,
}
```

It exists so the harness can deserialize the tool call arguments straight
into typed values. The submitted list is the FULL desired state — see
[full-state writes](plan.md#2-full-state-writes).

---

## Output types

### `TodoWriteOutput`

```rust
pub struct TodoWriteOutput {
    pub list: TodoList,     // the FULL new state after the write
    pub nags: Vec<Nag>,
}
```

The whole new list plus any advisory nags. The `Plan` wrapper replaces its
internal state with `output.list` automatically; callers of the free
function must do that themselves.

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
message that doubles as a correction prompt for the model. An `Err` means
the write was rejected and the list was left unchanged.
