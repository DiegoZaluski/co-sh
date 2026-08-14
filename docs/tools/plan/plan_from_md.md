# `plan_from_md` — parsing a Markdown plan file

`plan_load_from_md` turns a Markdown plan file into a [`TodoList`](types.md#todolist),
so a plan can be written by hand (or generated), saved with `fs_write`, and
then executed through the todo tools:

```rust,ignore
pub fn plan_from_md(path: &str) -> Result<TodoList, PlanError>
```

The wrapper method is `Plan::load_from_md(&mut self, path)` — it parses the
file and replaces the internal list. The one error case is an unreadable
file: `"Failed to read plan file: <io error>"`.

---

## The `PLAN_WRITE` syntax

The canonical syntax lives in the crate constant `plan::PLAN_WRITE`. Its
rules are few:

```markdown
# Plan: <title>                 ← any title line, ignored

## Group heading                ← one per group of tasks
- [ ] Task description          ← pending task
  - depends: task-1, task-3     ← optional, on a sub-bullet under the task
- [x] Completed task            ← completed task

## Another group
- [ ] More tasks
```

1. **Groups** start with `## ` (level-2 heading). The text after `## `
   becomes the group title. Everything before the first `## ` is ignored.
2. **Tasks** are flat checklist items: `- [ ] ` for pending, `- [x] ` or
   `- [X] ` for completed.
3. **Dependencies** go on a `- depends: a, b` sub-bullet *immediately after*
   the task; the list is comma-separated and each id is trimmed.
4. **Everything else is ignored**: blockquotes, code fences, regular
   paragraphs, deeper nesting, and the `# Plan:` title line.

IDs are assigned sequentially in the order tasks appear, across all groups:
the first task is `task-1`, the second `task-2`, and so on. Completed items
parse with status `Completed`, everything else `Pending`; `tests_verified`
starts `false` for every group.

---

## The parser's exact rules

The implementation is deliberately line-based and forgiving — it reads the
file line by line and only reacts to lines it recognizes:

| Line | Recognized as |
|---|---|
| `## <title>` | A new group (pushed, even if it ends up empty). |
| `- [ ] <desc>` | A pending task in the current group. |
| `- [x] <desc>` / `- [X] <desc>` | A completed task in the current group. |
| `- depends: <ids>` | The dependency list of the **last task added**. |
| anything else | Ignored. |

Subtleties worth knowing:

- The markers require the trailing space: `- [x]` alone (no space) or
  `- [x]Task` is **not** recognized. Use `- [x] Task`.
- Task lines before any `## ` heading are dropped (there is no group to put
  them in yet).
- A `- depends:` line attaches to the last task of the current group, even if
  a blank line separates them; an empty id list (`- depends:`) clears the
  dependencies.
- The title line (`# Plan: …`) and any prose are fine anywhere — they are
  ignored, which is what makes the file readable as normal Markdown.

---

## Example

A plan file, `plan.md`:

```markdown
# Plan: Ship the API

## Database
- [ ] Design schema
- [ ] Write migrations
  - depends: task-1

## API
- [x] Health check
```

Parsed:

```rust,ignore
use cosh_tools::plan::{TodoStatus, plan_from_md};

let list = plan_from_md("plan.md")?;
assert_eq!(list.groups.len(), 2);
assert_eq!(list.groups[0].title, "Database");
assert_eq!(list.groups[0].items[0].id, "task-1");
assert_eq!(list.groups[1].items[0].status, TodoStatus::Completed);

// The "Write migrations" task depends on "Design schema".
assert_eq!(list.groups[0].items[1].depends_on, vec!["task-1"]);
```

The typical loop is: `fs_write` the plan file → `plan_load_from_md` → read
the parsed list with [`todo_read`](todo_read.md) to verify it → drive the
work with `todo_write` / `todo_cross_off` / `todo_edit`.
