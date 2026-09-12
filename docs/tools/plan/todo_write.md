# `todo_write` — mutating the list

`todo_write` is the workhorse: six mutation actions under one function. All
of them follow the same shape — take the current [`TodoList`](types.md#todolist),
apply the action, and return the full new list plus any nags:

```rust,ignore
pub fn todo_write(list: &TodoList, action: &TodoWriteAction)
    -> Result<TodoWriteOutput, PlanError>
```

An `Err` means the action was rejected and **nothing changed**. A success
means the action was applied — but read the `nags`, because some problems are
tolerated rather than rejected (see [nags are advice, errors are
rejections](plan.md#1-nags-are-advice-errors-are-rejections)).

The [`Plan`](plan.md#the-plan-wrapper) wrapper method
`Plan::todo_write(&mut self, action)` applies the returned list to its
internal state automatically.

## `ReplaceList` — create the whole plan in one call

```rust,ignore
TodoWriteAction::ReplaceList {
    groups: vec![ReplaceGroup {
        title: "Database".into(),
        items: vec![
            ReplaceItem { key: Some("schema".into()), description: "Design schema".into(), depends_on: None },
            ReplaceItem { key: None, description: "Write migrations".into(), depends_on: Some(vec!["schema".into()]) },
        ],
    }],
}
```

Replaces the entire list with the given groups — the canonical way to create
a complete plan in a single call instead of adding tasks one at a time.

- The current list is discarded; the replacement gets `task-1..N` ids in
  listed order across all groups, all `Pending`, `tests_verified: false`.
- If the previous list was not empty, the output nags
  `"Replaced the existing TODO list (N tasks). Previous tasks are gone."`
- The optional per-item `key` is an alias other tasks **in the same call**
  may reference in `depends_on`; the tool resolves each alias to the real
  `task-N` id. Keys must not look like task ids (`task-<number>`): they are
  rejected to avoid silently shadowing the ids assigned by the same call.

**Errors:** an empty group list (`"ReplaceList requires at least one group..."`),
an empty description anywhere, or a duplicate `key`
(`"Duplicate key 'x' in ReplaceList; keys must be unique."`).

**Nags:** unknown dependency references (neither a sibling key nor an existing
id), self-references, and circular dependency chains are reported but the
list is still created.

---

## `Add` — append a task

```rust,ignore
TodoWriteAction::Add {
    group: "Database".into(),
    description: "Write migrations".into(),
    depends_on: Some(vec!["task-1".into()]),   // optional
}
```

- The task gets the next sequential id (`task-1`, `task-2`, … across all
  groups) and starts `Pending`.
- If a group with that title exists, the task is appended to it; otherwise a
  new group is created (with `tests_verified: false`).
- `depends_on` is stored as given, but problems with it only **nag**:

  | Dependency problem | Nag |
  |---|---|
  | `depends_on` contains the task's own id | `"Dependency 'x' is a self-reference."` |
  | `depends_on` references a task that does not exist | `"Dependency 'x' does not exist in the task list."` |

  The task is added either way — dependencies are declarative metadata, and
  the module trusts the caller to fix stale ones.

**Errors:** an empty or whitespace-only `description` is rejected
(`"Description must be non-empty text."`).

---

## `Start` — mark a task in-progress

```rust,ignore
TodoWriteAction::Start { id: "task-1".into() }
```

Sets the task's status to `InProgress`. Two rules matter here:

- **Only one task at a time.** If any *other* task is already `InProgress`,
  `Start` is rejected with `"Only one task at a time can be InProgress.
  Complete or cancel 'x' first before starting 'y'."` — this holds across
  groups, so you cannot have two parallel in-flight tasks.
- **Unmet dependencies nag but do not block.** If a dependency is not yet
  `Completed` (or no longer exists), the task still starts, with a nag like
  `"'task-2' depends on 'task-1' which is still Pending."` Starting a task
  early is your call; the module just tells you.

**Errors:** a nonexistent id is rejected
(`"Task 'x' does not exist. Use List to see available tasks."`).

---

## `Remove` — delete a task

```rust,ignore
TodoWriteAction::Remove { id: "task-1".into() }
```

Deletes the task wherever it lives. If other tasks still list it in their
`depends_on`, you get a nag — `"'task-1' was removed but is still referenced
as a dependency by 'task-2'."` — and the stale references remain. Clean them
up with [`todo_edit`](todo_edit.md) afterwards.

**Errors:** a nonexistent id is rejected.

---

## `Clean` — sweep terminal tasks

```rust,ignore
TodoWriteAction::Clean { keep_pending: true }
```

Removes finished work so the list reflects only what is left. The
`keep_pending` flag decides how aggressive the sweep is:

| `keep_pending` | Removed | Kept |
|---|---|---|
| `true` | `Completed`, `Cancelled` | `Pending`, `InProgress` |
| `false` | `Completed`, `Cancelled`, **and** `Pending` | only `InProgress` |

After removing items, groups that became empty are dropped too. `Clean` never
errors; it reports what it did through nags:

- `"Removed N completed or cancelled tasks."` (when N > 0)
- `"Removed M empty group(s)."` (when M > 0, pluralized)

---

## `VerifyGroup` — record that tests passed

```rust,ignore
TodoWriteAction::VerifyGroup { group: "Database".into() }
```

Sets the group's `tests_verified` flag to `true` — the model's statement that
tests were run for this group (see the [verification
contract](plan.md#2-the-verification-contract)). The flag persists: adding
more tasks to the group later does not un-verify it.

- If the group still has `Pending` or `InProgress` tasks, the verification is
  still recorded, but you get the nag `"Group 'Database' still has pending or
  in-progress tasks."`
- **Errors:** a nonexistent group is rejected (`"Group 'x' does not exist."`).

---

## Example

```rust,ignore
use cosh_tools::plan::{TodoList, TodoWriteAction, todo_write};

let mut list = TodoList::default();

// Add two tasks; the second depends on the first.
list = todo_write(&list, &TodoWriteAction::Add {
    group: "Database".into(),
    description: "Design schema".into(),
    depends_on: None,
})?.list;
let out = todo_write(&list, &TodoWriteAction::Add {
    group: "Database".into(),
    description: "Write migrations".into(),
    depends_on: Some(vec!["task-1".into()]),
})?;
list = out.list;

// Start the first task.
list = todo_write(&list, &TodoWriteAction::Start { id: "task-1".into() })?.list;

// A second Start is rejected: only one task can be in progress.
assert!(todo_write(&list, &TodoWriteAction::Start { id: "task-2".into() }).is_err());
```

Note the free-function pattern: each call takes the list and you keep the
returned `list`. The [`Plan`](plan.md#the-plan-wrapper) wrapper does this
bookkeeping for you.
