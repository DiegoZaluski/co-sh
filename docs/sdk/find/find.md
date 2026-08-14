# The `find` module: filesystem search engines

`find` is the **search engine layer** of the SDK: it finds files by *name*
([`glob`](glob.md)) and by *content* ([`grep`](grep.md)), backed by a shared
[filesystem scan cache](fs_cache.md) and a [cancellation model](task.md). It
is the engine that powers the `find` tools in `cosh-tools` (`find_glob`,
`find_grep`, and the `fs_ast_edit` file discovery).

```
            ┌─────────────┐     glob(pattern, path)     ┌──────────────┐
   name ───▶│    glob     │────────────────────────────▶│  GlobResult  │
            └──────┬──────┘                            └──────────────┘
                   │  shared entry scan cache (fs_cache)
            ┌──────┴──────┐     grep(pattern, path)     ┌──────────────┐
 content ──▶│    grep     │────────────────────────────▶│  GrepResult  │
            └─────────────┘                            └──────────────┘
```

Both functions are **synchronous** — they block the calling thread and return
a complete result. Long runs are bounded by an optional `timeout_ms`; a
timeout that trips *mid-search* is not an error (see below).

## The two engines

| | [`glob`](glob.md) | [`grep`](grep.md) |
|---|---|---|
| **Answers** | "where is the file?" | "which line says X?" |
| **Matches on** | file/dir **names** (glob patterns) | file **contents** (regexes) |
| **Output** | matched entries (path, type, mtime, size) | matched lines + context, per-file counts |
| **Output modes** | — | `Content`, `Count`, `FilesWithMatches` |

Both take an options struct, both return `Result<_, String>` with descriptive
error strings, and both accept an optional `on_match` callback that streams
matches live as the parallel walk finds them.

## Shared machinery

The two engines share three building blocks:

1. **[`fs_cache`](fs_cache.md)** — a TTL-based cache of scanned directory
   entries. Pass `cache: true` to reuse a recent scan across calls instead of
   re-walking the tree; mutations invalidate it automatically via
   `invalidate_path` / `invalidate_all`.
2. **[`task`](task.md)** — cooperative cancellation. Every operation runs
   under a [`CancelToken`]; a timeout or an external abort stops the walk
   promptly.
3. **[`glob_util`](glob_util.md)** — pattern compilation helpers shared by
   both engines (including the `**/` recursive prefix and brace fixing).

## The timeout contract (read this once)

Timeouts are handled with a single consistent rule across the module:

- A timeout that elapses **before any work is salvaged** (before the walk
  starts) is an **error** — nothing was found, so there is nothing to return.
- A timeout that trips **mid-walk** keeps the partial results collected so
  far and returns them with `timed_out: true` — an *empty* partial is an
  **incomplete scan**, not proof of absence.

So a caller that gets `timed_out: true` should scope the search deeper rather
than conclude "nothing matches".

## Visibility defaults

Hidden-file handling differs between the engines, so check the per-engine
docs rather than assuming one rule:

- **`glob` defaults `hidden` to `false`** — `.`-prefixed entries are excluded
  unless you opt in.
- **`grep` defaults `hidden` to `true`** — hidden files are searched by
  default (content search is expected to be exhaustive).

Both engines prune `node_modules` unless the pattern mentions it, and `.git`
is **always** skipped by the walker. (The `cosh-tools` wrappers default both
engines to `hidden: true` for the model's benefit — the SDK keeps its own,
per-engine defaults.)

## Vendored origin

The module is vendored from oh-my-pi's native tooling and stripped of its
N-API/JS surface, so the naming and option shapes deliberately match that
lineage (`GrepOptions.r#type`, `max_columns`, and so on).

---

## Example

A complete runnable walkthrough lives at
[`examples/find/find.rs`](../../../crates/cosh-sdk/examples/find/find.rs): it
builds a scratch project, searches it by name and by content, exercises the
output modes and the scan cache, and shows the error paths.

Next: [glob — searching by name](glob.md), then [grep — searching by
content](grep.md).
