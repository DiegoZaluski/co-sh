# The `plan` module: a stateful TODO list for multi-step work

`plan` is a small state machine for long-running tasks. It keeps a structured
TODO list — a flat list of tasks with statuses and dependencies — that the
agent rewrites as work progresses, instead of losing the plan in conversation
history. It is the one cosh tool that is fully **stateful**: the list lives on
a single in-memory structure that the harness owns and mirrors into the
model's context.

| Tool | What it does |
|---|---|
| [`plan_todo_write`](todo_write.md) | Write the FULL TODO list (full-state replacement: statuses, dependencies, clearing). |

There is exactly one tool, and it follows the dominant trained pattern
(Claude Code's `TodoWrite`): a single write tool that receives the complete
list on every call. There is no read tool, no edit tool, no cross-off tool:
changing a status is just writing the list again, and the harness always shows
the model the current plan (see [the protected context
block](#the-harness-and-the-protected-context-block)), so a read would be
redundant.

The design mirrors the other cosh modules: the public surface is a
builder-style wrapper — [`Plan`](#the-plan-wrapper) — that holds the list and
exposes one stateful method, plus the **pure free function** (`todo_write`)
that produces the new list from the submitted one; the wrapper is a thin
stateful shell around it.

---

## The `Plan` wrapper

```rust,ignore
use cosh_tools::plan::{Plan, TodoWriteInput};

let mut plan = Plan::new();                       // an empty list

let input: TodoWriteInput = serde_json::from_value(serde_json::json!({
    "todos": [
        { "description": "Design schema", "key": "schema" },
        { "description": "Write migrations", "depends_on": ["schema"] }
    ]
}))?;
plan.todo_write(&input.todos)?;                   // full-state write
```

The wrapper's method takes `&mut self`: it applies the returned list to the
internal state, so callers never thread the list around. `Plan::default()` is
`Plan::new()`, and `plan.list()` borrows the current
[`TodoList`](types.md#todolist) for inspection.

### Why `&mut self`

The wrapper is the API the harness uses, and the harness keeps its `Plan`
behind a `Mutex`. That single shared instance is what makes the TODO list
**the plan** — every `plan_todo_write` call mutates the same state, and the
harness mirrors that state into the model's context after every call (see
[below](#the-harness-and-the-protected-context-block)). There is no
persistence: the list lives for the session.

---

## The two core concepts

### 1. Nags are advice, errors are rejections

Every operation returns either `Err(PlanError)` — the request was **rejected
and nothing changed** — or an output struct that contains `nags: Vec<Nag>`.
Nags are advisory messages about problems that are **tolerated**: the
operation still succeeds, and the caller is expected to read the nags and fix
the situation.

| Concern | Becomes | Example |
|---|---|---|
| Empty task description | **error** — a task must have text | `description: ""` |
| Duplicate keys / keys shaped like `task-N` | **error** — they would shadow real ids | two items `key: "schema"` |
| Two tasks in progress at once | **error** — the invariant is enforced | two items `status: "in_progress"` |
| Dependency is missing / self / cyclic | **nag** — the list is still updated | `depends_on: ["nope"]` |
| An in-progress task with unfinished dependencies | **nag** — the state is accepted | `in_progress` on a task whose dep is `pending` |

So when an operation succeeds, **check `output.nags` before trusting the
result fully** — a task may have been written with a dependency that does not
exist.

### 2. Full-state writes

Every `plan_todo_write` call replaces the previous list entirely. To start,
complete, or cancel a task, the model rewrites the whole list with the updated
status — the same move for every transition, matching the pattern the models
were trained on. Exactly one task may be `in_progress` at a time; an empty
list clears the plan.

---

## Ids, keys, and dependencies

Ids are assigned by the tool, not the caller: tasks get `task-1..N` in listed
order. Items may carry an optional `key` alias that sibling tasks reference in
`depends_on` within the same call — the tool resolves each alias to the real
`task-N` id. Existing `task-N` ids are accepted as dependencies too. Unknown
keys and cycles are reported as nags; duplicate keys are errors.

---

## The harness and the protected context block

Two harness behaviors shape how the plan is actually used:

- **The plan is always visible.** The harness renders the current list as a
  dedicated, protected `## Tool TODOs` context block injected at the END of
  the message list — right where the model is about to generate — and
  re-renders it after every `plan_*` call. The block survives every
  compaction phase and disappears only when all tasks are terminal or the
  list is empty. The model therefore always sees the plan and never needs a
  read tool.
- **Ask mode has no plan tools.** The mutating tool is restricted in Ask
  mode, and since reading is unnecessary by design, no plan tool is exposed
  there at all.

---

## Summary

- One stateful `Plan` per session; one full-state write operation with a pure
  free function underneath.
- **Errors reject and change nothing; nags advise and tolerate.** Check
  `output.nags` after every write.
- Tasks get sequential `task-1`, `task-2`, … ids; at most one task may be
  `in_progress` at a time.
- To change any status, rewrite the full list; call with an empty list to
  clear the plan.

Next: the [data model](types.md), then the [operation page](todo_write.md).
