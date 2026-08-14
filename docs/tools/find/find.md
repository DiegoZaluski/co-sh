# The `find` module: locating files and searching content

This module gives an agent the two complementary ways to *find things* in a
project:

| Tool | What it does |
|---|---|
| [`glob`](glob.md) | Find files and directories by **name** — a glob pattern like `**/*.rs`. |
| [`grep`](grep.md) | Search **file contents** for lines matching a regex. |

Both are built on the `cosh-sdk` find engine and share a single configuration
wrapper, [`Find`](#the-find-wrapper), so one configured instance can dispatch
either search. Every result is shaped to keep the model's context bounded and
to feed the [`fs`](../fs/fs.md) tools directly.

The module mirrors the `fs` design: a builder-style wrapper holds shared
state — the project root and the path guards — and the operations are thin
methods on it. This page covers the wrapper, the shared output contract, and
how the two tools differ. The [type reference](types.md) documents the input
and output data structures, and each tool has its own page with examples.

---

## The `Find` wrapper

`Find` is a builder-style struct holding configuration for both searches. Like
`Fs`, you configure it once and then call operations:

```rust,ignore
use cosh_tools::find::Find;

let find = Find::new()
    .cwd("/home/user/project")   // the project root (path guards)
    .max_results(50)
    .gitignore(true);

find.glob("**/*.rs", "src")?;    // files by name
find.grep("fn main", "src")?;    // lines by content
```

Unlike `Fs`'s methods, `Find`'s operations are **synchronous**: `glob` and
`grep` return `Result<_, String>` directly rather than awaiting.

`Find::new()` starts with no root set; until you call [`cwd`](Find::cwd) every
path is denied. `Find` also implements `Default`.

The methods split into three groups:

| Group | Methods | Meaning |
|---|---|---|
| Glob | `glob`, `glob_with`, `glob_full` | Find by name, with escalating option bundles. |
| Grep | `grep`, `grep_skipping`, `grep_with`, `grep_with_streaming` | Search content, with pagination/streaming variants. |
| Guards | `cwd`, `allowlist`, `blocklist`, `add_allowlist_path`, `remove_allowlist_path`, `find_root`, `allowlist_ref`, `blocklist_ref` | Project scope and permission policy. |

Shared builder options affect both tools:

- `hidden` — include `.`-prefixed entries. **Default `true`**; `.git` is
  *always* excluded regardless.
- `gitignore` — respect `.gitignore` rules. **Default `true`**.
- `timeout_ms` — abort after this many ms (**default 5000**) and return the
  partial results instead of failing.
- `max_results` — result cap for glob (**default and ceiling 200**, can only
  lower). Grep bounds its own window; see [grep](grep.md#output-bounds).

Glob-only: `file_type` (`"file"`/`"dir"`/`"symlink"`), `glob_format`
(`"flat"`/`"grouped"`/`"tree"`), `sort_by_mtime` (default `true` — most recent
first).

Grep-only: `name_glob` (restrict by file-name glob), `language` (restrict by
file type like `"rust"`), `ignore_case`, `max_count`, `context_before`,
`context_after`.

As with `Fs`, `Find` carries ready-to-serve MCP tool descriptions in the public
fields `description_glob` and `description_grep`.

### Path guards

`Find` uses the same [`PathGuard`](../fs/fs.md#the-path-guard-permission-model)
permission model as `fs`. Each search target is resolved individually against
the root, allowlist, and blocklist, so the approval shown to the user is
exactly the set of paths the search opens. The same rules apply: allowlist
entries match **exactly**, blocklist entries match by **prefix**, and a path in
both is ambiguous.

---

## Shared output contract

Both tools agree on the important semantic: **a timeout is not an error, and an
empty result is never silently assumed to mean "nothing exists".**

| Situation | What you see |
|---|---|
| Timeout expired | `timed_out: true`; `matches` holds the partial results. An *empty* timed-out result is an **incomplete scan**, not proof of absence — narrow the scope instead of retrying blindly. |
| No matches (no timeout) | `useless: true` with a `note` explaining what was searched. The result carries no new information — adjust the pattern or scope. |
| Cap cut the list | `limit_reached: true` (glob) / `file_limit_reached` or `per_file_limit_reached` (grep); a `note` tells you how to continue. |

Both return a human-readable `note` field alongside the structured data; the
model should read `note` before acting.

### Feeding the `fs` tools

The two modules are designed to chain. Grep output carries a
**hashline anchor** per anchored file (the first 20 shown): the path and tag
can be passed straight to `fs_edit` to edit a matched file *without re-reading
it* — the anchor was minted from the same whole-file snapshot the edit engine
validates against. Glob output rebases paths relative to the working directory
so they match the paths `fs` reports.

---

## How the two tools differ

- **`glob` matches names** — paths, directories, and symlinks. It answers
  "where is the code?". Patterns are case-sensitive glob syntax with
  `**`/`*`/`?`/`[...]`/`{a,b}`.
- **`grep` matches content** — it answers "which line talks about X?". Patterns
  are regular expressions. Regex syntax errors are reported as errors.
- **Glob is quiet** about matches it skips (missing targets are reported in
  `missing_paths`); **grep always reports** how many files it searched.

---

## Summary

- Configure one `Find`, call `glob` or `grep` — synchronous `Result`s.
- A timeout returns partials; an empty timed-out result is incomplete, not proof
  of absence; an untimed zero-match result is `useless` — don't blindly retry.
- Grep gives you hashline anchors so you can edit matches directly.

Next: the [data types](types.md), then the two tools — [glob](glob.md) and
[grep](grep.md).