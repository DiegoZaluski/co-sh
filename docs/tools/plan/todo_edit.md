# `todo_edit` — editing task metadata

`todo_edit` changes the metadata of an existing task: its description, its
group, and/or its dependency list. The edit is **partial** — every field on
[`TodoEdit`](types.md#todoedit-and-todocrossoff) is `Option`, and only the
fields you provide are touched:

```rust,ignore
pub fn todo_edit(list: &TodoList, edit: &TodoEdit)
    -> Result<TodoWriteOutput, PlanError>
```

```rust,ignore
let edit = TodoEdit {
    id: "task-1".into(),
    description: Some("Design the schema (v2)".into()),  // rewrite
    group: Some("Backend".into()),                       // move to another group
    depends_on: None,                                    // leave unchanged
};
```

The wrapper method is `Plan::todo_edit(&mut self, edit)`.

---

## What each field does

| Field | Effect |
|---|---|
| `description: Some(text)` | Replaces the description. Empty/whitespace text is **rejected** (`"Description must be non-empty text."`). |
| `group: Some(title)` | Moves the task to that group, creating it if needed (new groups start `tests_verified: false`). A group equal to the current one is a no-op. |
| `depends_on: Some(ids)` | Replaces the whole dependency list. |

Fields left `None` are untouched — that is what makes `TodoEdit` safe to
reuse: one struct can describe a rename, a move, or both without needing to
know the task's current values.

---

## Dependency problems nag (and are still applied)

Like [`Add`](todo_write.md#add--append-a-task), an edited `depends_on` list is
stored **as given**, and problems only produce nags:

| Problem | Nag |
|---|---|
| `depends_on` contains the task's own id | `"Dependency 'x' is a self-reference."` |
| `depends_on` references a nonexistent task | `"Dependency 'x' does not exist in the task list."` |
| The dependency would create a **cycle** | `"Dependency 'x' would create a circular dependency chain."` |

The cycle check is transitive: it walks the dependency graph from the new
dependency to see whether the edited task is already reachable from it
(A → B → C, then editing A to depend on C is caught). Despite the nag, the
list is updated — the module warns, it does not police.

---

## Errors

- Nonexistent task id → `"Task 'x' does not exist. Use List to see available
  tasks."`
- Empty new description → `"Description must be non-empty text."`

Both reject the whole edit: nothing changes.

---

## Example

```rust,ignore
use cosh_tools::plan::{TodoList, TodoWriteAction, TodoEdit, todo_write, todo_edit};

let mut list = TodoList::default();
list = todo_write(&list, &TodoWriteAction::Add {
    group: "Database".into(),
    description: "Design schema".into(),
    depends_on: None,
})?.list;

// Rename the task and move it to the "Backend" group in one edit.
let out = todo_edit(&list, &TodoEdit {
    id: "task-1".into(),
    description: Some("Design schema (v2)".into()),
    group: Some("Backend".into()),
    depends_on: None,
})?;
list = out.list;

// A circular dependency nags — task-1 now depends on task-2, and task-2
// depends on task-1 — but the edit still lands.
let list2 = todo_write(&list, &TodoWriteAction::Add {
    group: "Backend".into(),
    description: "Migrations".into(),
    depends_on: Some(vec!["task-1".into()]),
})?.list;
let out = todo_edit(&list2, &TodoEdit {
    id: "task-1".into(),
    description: None,
    group: None,
    depends_on: Some(vec!["task-2".into()]),
})?;
assert!(out.nags.iter().any(|n| n.message.contains("circular")));
```
