# The `plan` module: a stateful TODO list for multi-step work

`plan` is a small state machine for long-running tasks. It keeps a structured
TODO list — groups of tasks with statuses and dependencies — that the agent
mutates as work progresses, instead of losing the plan in conversation
history. It is the one cosh tool that is fully **stateful**: every mutation
happens on a single in-memory list that the harness owns and mirrors into the
model's context.

| Tool | What it does |
|---|---|
| [`plan_todo_write`](todo_write.md) | Mutate the list: `Add`, `Start`, `Remove`, `Clean`, `VerifyGroup`. |
| [`plan_todo_edit`](todo_edit.md) | Edit an existing task's metadata (description, group, dependencies). |
| [`plan_todo_cross_off`](todo_cross_off.md) | Mark a task `Completed` or `Cancelled`. |
| [`plan_todo_read`](todo_read.md) | Query the list: list with optional filters, or fetch one task. |
| [`plan_load_from_md`](plan_from_md.md) | Parse a Markdown plan file into the list. |

The design mirrors the other cosh modules: the public surface is a
builder-style wrapper — [`Plan`](#the-plan-wrapper) — that holds the list and
exposes one method per operation. Each operation also exists as a **pure free
function** (`todo_write`, `todo_edit`, …) that works on an explicit
[`TodoList`](types.md#todolist) and returns the new list in its output; the
wrapper is a thin stateful shell around them.

---

## The `Plan` wrapper

```rust,ignore
use cosh_tools::plan::{Plan, TodoReadAction, TodoWriteAction, TodoCrossOff};

let mut plan = Plan::new();                       // an empty list

plan.todo_write(&TodoWriteAction::Add {
    group: "Database".into(),
    description: "Design schema".into(),
    depends_on: None,
})?;

plan.todo_write(&TodoWriteAction::Add {
    group: "Database".into(),
    description: "Write migrations".into(),
    depends_on: Some(vec!["task-1".into()]),
})?;

plan.todo_cross_off(&TodoCrossOff::Complete { id: "task-1".into() })?;

let out = plan.todo_read(&TodoReadAction::List { group: None, status: None })?;
```

The wrapper's methods mirror the free functions' signatures but take `&mut
self`: each one applies the returned list to the internal state, so callers
never thread the list around. `Plan::default()` is `Plan::new()`, and
`plan.list()` borrows the current [`TodoList`](types.md#todolist) for
inspection.

### Why `&mut self`

The wrapper is the API the harness uses, and the harness keeps its `Plan`
behind a `Mutex`. That single shared instance is what makes the TODO list
**the plan** — every `plan_*` call mutates the same state, and the harness
mirrors that state into the model's context after every call (see
[below](#the-harness-and-the-protected-context-block)). There is no
persistence: the list lives for the session.

### `with_test`

`Plan::with_test(value)` is a configuration knob on the wrapper — "whether
test generation is expected after each completed group". It currently has no
observable effect on the todo operations themselves (there is no public
getter and the harness does not set it today); it is documented here for
completeness as part of the public API surface.

---

## The two core concepts

Everything else in the module is easiest to understand through two ideas that
recur on every operation page.

### 1. Nags are advice, errors are rejections

Every operation returns either `Err(PlanError)` — the request was **rejected
and nothing changed** — or an output struct that contains `nags: Vec<Nag>`.
Nags are advisory messages about problems that are **tolerated**: the
operation still succeeds, and the caller is expected to read the nags and fix
the situation. The same concern is sometimes a nag and sometimes an error,
depending on whether it makes the operation meaningless:

| Concern | Becomes | Example |
|---|---|---|
| Empty task description | **error** — a task must have text | `Add { description: "" }` |
| Task / group does not exist | **error** — nothing to act on | `Start { id: "ghost" }` |
| Task already in a terminal state | **error** — nothing to do | `Complete` on a completed task |
| Two tasks in progress at once | **error** — the invariant is enforced | `Start` while another is `InProgress` |
| Dependency is missing / self / cyclic | **nag** — the list is still updated | `Add` with `depends_on: ["nope"]` |
| A removed task is still referenced | **nag** — the stale reference remains | `Remove` a dependency |
| A group still has pending work when verified | **nag** — verification still recorded | `VerifyGroup` with pending tasks |

So when an operation succeeds, **check `output.nags` before trusting the
result fully** — a task may have been added with a dependency that does not
exist.

### 2. The verification contract

Each [`TaskGroup`](types.md#taskgroup) carries a `tests_verified: bool` flag.
The intended workflow is: complete a group's tasks, run the tests, then call
`VerifyGroup` to record that tests passed. The module enforces this gently:

- `todo_read` emits a **verification nag** for any group whose tasks are all
  terminal but that has not been verified yet:
  `"Group 'X' is complete but tests have not been confirmed. Use VerifyGroup
  to confirm tests passed."`
- `VerifyGroup` sets the flag (and nags if the group still has pending or
  in-progress tasks — verification is recorded anyway).
- Adding new tasks to a verified group does **not** un-verify it.

---

## The Markdown plan format

Plans can be written by hand as Markdown files and loaded with
`plan_load_from_md`. The canonical syntax is defined by the crate constant
[`PLAN_WRITE`](plan_from_md.md#the-plan_write-syntax) — group headings, flat
checklist items, and optional `depends:` sub-bullets. The full grammar and
its edge cases are on the [parser page](plan_from_md.md).

---

## The harness and the protected context block

Two harness behaviors shape how the plan is actually used:

- **The plan is always visible.** The harness renders the current list as a
  dedicated, protected `## Tool TODOs` context block injected right after the
  system prompt — before the conversation history — and re-renders it after
  every `plan_*` call. The block survives every compaction phase (draft
  eviction, LLM summarization, overflow drain) and disappears only when all
  tasks are terminal or the list is empty. The model therefore sees the plan
  without calling `plan_todo_read` first, and cannot forget it.
- **Ask mode is read-only.** In Ask mode only `plan_todo_read` and
  `plan_load_from_md` are exposed; the mutating tools (`plan_todo_write`,
  `plan_todo_edit`) are restricted. (`plan_todo_cross_off` mutates too, so it
  is not exposed in Ask mode either.)

---

## Summary

- One stateful `Plan` per session; five operations, each with a pure free
  function and a stateful wrapper method.
- **Errors reject and change nothing; nags advise and tolerate.** Check
  `output.nags` after every mutation.
- Tasks get sequential `task-1`, `task-2`, … ids; at most one task may be
  `InProgress` at a time; terminal states (`Completed`/`Cancelled`) are
  sticky.
- Groups must be verified with `VerifyGroup` after their tests pass, or
  `todo_read` will nag about them.

Next: the [data model](types.md), then the five operation pages.
