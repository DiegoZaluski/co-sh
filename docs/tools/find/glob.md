# `glob` — finding files and directories by name

`glob` finds filesystem entries whose *names* match a glob pattern. It answers
"where is the code?" — find every `.rs` file, every test directory, every lock
file — and returns a bounded, sorted list you can feed to a `read` or `edit`.

```
Find::glob(&self, pattern: &str, path: &str) -> Result<GlobOutput, String>
```

`glob` is synchronous. For a single search it is the simplest call; the richer
`glob_with` / `glob_full` variants add multiple targets and the full option
bundle (see the [type reference](types.md#globentry-and-globoutput)).

---

## How targets and patterns combine

A `glob` call is really two inputs: a `pattern` and one or more *targets*
(`path`/`paths`). **The shape of each target decides the semantics** — a target
may be a glob, a directory literal, or a file literal:

| Target | Behavior |
|---|---|
| `*.rs` | **Bare glob** — searched recursively across the target base (the working directory when no `path` is given). Equivalent to `**/*.rs`. |
| `src/*.rs` | **Scoped glob** — stays shallow: matches `src/*.rs` only, *not* `src/sub/x.rs`. |
| `src/**/*.rs` | **Already-recursive glob** — unchanged. |
| `src` | **Directory literal** — lists everything under it recursively. The `pattern` argument applies within the directory. |
| `foo.rs` | **File literal** — the file itself is returned as the single match. |

Two things to notice:

- **A bare glob recurses; a scoped glob does not.** `*.rs` at the root finds
  files at every depth; `src/*.rs` is deliberately shallow.
- **When the target is a plain directory, the `pattern` argument is used
  as-is — and it is shallow unless it contains `**/`.** With `path: "src"`,
  `pattern: "*.rs"` matches only the *direct* children of `src`; use
  `pattern: "**/*.rs"` for the whole tree.

`paths` overrides `path` when present and lets you search several targets in
one call; each is walked as its own root and the results are merged, deduped,
and (by default) re-ranked by modification time so the global top-N is correct.

---

## Options

| Option | Default | Effect |
|---|---|---|
| `file_type` | all kinds | Restrict results to `"file"`, `"dir"`, or `"symlink"`. |
| `hidden` | `true` | Include `.`-prefixed entries. `.git` is **always** excluded regardless. |
| `gitignore` | `true` | Respect `.gitignore` rules; set `false` to include ignored files. |
| `max_results` | `200` (ceiling `200`) | Cap the result count. Can only **lower** the cap — a value above 200 is clamped down. |
| `sort_by_mtime` | `true` | Sort by modification time, most recent first. This is also what populates each entry's `mtime_ms`/`size_bytes` metadata: with `sort_by_mtime: false` the scan reads minimal metadata and those fields are `None`. |
| `format` | none | Render the `formatted` field as `"flat"`, `"grouped"`, or `"tree"`. |
| `timeout_ms` | `5000` | Abort after this many ms and return partials with `timed_out: true`. |

`recursive` is not consulted by the glob engine: recursion is **always**
derived from the target's shape above, never from a flag. To recurse, use a
bare glob, a `**/` pattern, or a directory literal; to stay shallow, use a
scoped `dir/*` glob. Paths in the output are rebased relative to the working
directory (directories get a trailing `/`), matching the path form the `fs`
tools report.

---

## Reading the result

`GlobOutput` carries the structured `matches` plus a few flags that matter for
how you *use* the result:

- `useless: true` + note — zero matches (no timeout). The result carries no new
  information; adjust the pattern or scope rather than retrying blindly.
- `limit_reached: true` + note — the cap cut the list; more entries may exist.
  Narrow the pattern or add `file_type` to reduce results.
- `timed_out: true` — partial results. An *empty* timed-out result is an
  **incomplete scan**, not proof of absence. Scope to a deeper directory
  (e.g. `sub/dir/*.ext`) instead of retrying at a huge root.
- `missing_paths` — targets that were missing on disk were skipped; the call
  only errors when *every* target is missing (`Path not found: ...`).

The `formatted` field always holds the paths rendered per the requested
`format` (`flat`/`grouped`/`tree`), so the model never has to reconstruct
grouping itself.

---

## Errors and edge cases

| Situation | What happens |
|---|---|
| No targets, or every target missing | Hard `Err` (`no search targets provided` / `Path not found: ...`). |
| Invalid glob pattern | Hard `Err` (`Invalid glob pattern: ...`). |
| Unknown `file_type` / `format` | Hard `Err` naming the accepted values. |
| `max_results: 0` | Hard `Err` (`max_results must be a positive number`). |
| Missing target among several | Skipped; reported in `missing_paths`; survivors still returned. |
| Timeout | Partial results with `timed_out: true`, never a blind error. |

---

## Example

A complete runnable example is provided at
[`examples/find/glob.rs`](../../../crates/cosh-tools/examples/find/glob.rs).
It builds a scratch project and demonstrates the target shapes above — bare
glob, scoped glob, directory literal, `file_type` filtering, `tree` formatting,
and multi-target search.

Next: [grep — searching file contents](grep.md).