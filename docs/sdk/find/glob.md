# `find::glob` — searching by name

`glob` finds filesystem entries whose *names* match a glob pattern.

```rust,ignore
pub fn glob(options: GlobOptions) -> Result<GlobResult, String>
```

One call, one options struct:

```rust,ignore
pub struct GlobOptions {
    pub pattern: String,              // e.g. "*.rs" (empty string means "*")
    pub path: String,                 // directory to search (required)
    pub file_type: Option<FileType>,  // "file" | "dir" | "symlink"
    pub recursive: Option<bool>,      // default: true
    pub hidden: Option<bool>,         // default: false
    pub max_results: Option<u32>,     // default: unbounded
    pub gitignore: Option<bool>,      // default: true
    pub cache: Option<bool>,          // use the shared scan cache (default: false)
    pub sort_by_mtime: Option<bool>,  // default: false
    pub include_node_modules: Option<bool>,
    pub timeout_ms: Option<u32>,
    pub on_match: Option<Arc<GlobMatchCallback>>,  // live streaming
}
```

## How a pattern is interpreted

The pattern goes through [`build_glob_pattern`](glob_util.md) first:

- **Simple patterns get a `**/` prefix** — `"*.rs"` becomes `"**/*.rs"`, so
  it matches at *every* depth under `path`. This is the default (`recursive:
  true`) behavior.
- **Patterns that already contain a `/` are left alone** — `"src/*.rs"` stays
  shallow, matching `src/lib.rs` but not `src/sub/lib.rs`. Use
  `"src/**/*.rs"` for the whole tree.
- **`recursive: false` disables the prefix** — `"*.rs"` then matches only the
  direct children of `path`.
- **Unclosed `{` alternation groups are auto-closed** — `"*.{ts,tsx"` is
  repaired to `"*.{ts,tsx}"` instead of erroring (a tolerance for LLM-written
  patterns; see [glob_util](glob_util.md)).

A pattern that is empty or whitespace becomes `"*"`.

## Filtering semantics

| Option | Default | Effect |
|---|---|---|
| `file_type` | none | Only `File`, `Dir`, or `Symlink` entries. A symlink is matched for `file`/`dir` by resolving its **target** type. |
| `hidden` | `false` | Include `.`-prefixed entries. `.git` is always skipped regardless. |
| `gitignore` | `true` | Respect `.gitignore`; set `false` for exhaustive traversal. |
| `include_node_modules` | depends | `node_modules` is pruned unless `true` or the pattern mentions it. |
| `sort_by_mtime` | `false` | Rank by modification time, most recent first (path tiebreak). Only with `sort_by_mtime` is the result ordered; otherwise it follows walk order. |
| `max_results` | unbounded | Cap the result count. With `sort_by_mtime`, only the current top-N is kept while walking; without it, matching stops early at the cap. |

`on_match` fires for every match *as the walk finds it* (before the mtime rank
and limit), on walker worker threads — keep it cheap and `Sync`.

## The result

```rust,ignore
pub struct GlobResult {
    pub matches: Vec<GlobMatch>,   // path (relative, forward slashes), file_type, mtime (ms), size
    pub total_matches: u32,        // matches.len(), clamped
    pub timed_out: bool,           // partial results — scan was cut short
}
```

Paths are **relative to the searched directory**, using forward slashes even
on Windows. Directories report `size: None`; the trailing `/` is **not**
added (unlike the cosh-tools wrapper, which renders one).

## Errors

| Situation | What happens |
|---|---|
| `path` missing / unresolvable | `Path not found: ...` |
| `path` is not a directory | `Search path must be a directory` |
| Invalid glob pattern | `Invalid glob pattern: ...` / `Failed to build glob matcher: ...` |
| Timeout before the walk starts | `Timeout` (hard error — nothing to salvage) |
| Mid-walk timeout | Partial `GlobResult` with `timed_out: true` |

---

Next: [glob_util — pattern helpers](glob_util.md), or [grep — searching by
content](grep.md).
