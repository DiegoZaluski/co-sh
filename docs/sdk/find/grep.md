# `find::grep` — searching by content

`grep` searches file *contents* with ripgrep's regex engine, across a
directory (walked in parallel) or a single file (searched in memory).

```rust,ignore
pub fn grep(options: GrepOptions) -> Result<GrepResult, String>
```

## Options

```rust,ignore
pub struct GrepOptions {
    pub pattern: String,              // regex (ripgrep syntax)
    pub path: String,                 // file or directory
    pub glob: Option<String>,         // filename glob filter, e.g. "*.ts"
    pub r#type: Option<String>,       // language type filter, e.g. "rust", "py"
    pub ignore_case: Option<bool>,
    pub multiline: Option<bool>,
    pub hidden: Option<bool>,         // default: true  (SDK default — see find.md)
    pub gitignore: Option<bool>,      // default: true
    pub cache: Option<bool>,          // use the shared scan cache
    pub max_count: Option<u32>,       // global match cap
    pub offset: Option<u32>,          // skip the first N matches
    pub context_before: Option<u32>,
    pub context_after: Option<u32>,
    pub context: Option<u32>,         // legacy: sets both before and after
    pub max_columns: Option<u32>,     // truncate long lines
    pub mode: Option<GrepOutputMode>, // Content | Count | FilesWithMatches
    pub max_count_per_file: Option<u32>,  // per-file cap (content mode)
    pub timeout_ms: Option<u32>,
    pub on_match: Option<Arc<GrepMatchCallback>>,  // live streaming
}
```

## Output modes

| Mode | `matches` contains | `total_matches` |
|---|---|---|
| `Content` (default) | matched lines with `line_number`, `line`, context, `truncated` | total matched lines |
| `Count` | one row per matched file with `match_count` (`line_number: 0`, empty `line`) | total matches across files |
| `FilesWithMatches` | one row per matched file (no line content) | number of matched files |

## Limiting and pagination

- `max_count` caps the **global** number of emitted matches/files
  (count-mode applies it to matches, not files). `limit_reached` reports
  whether the cap cut the search short.
- `offset` skips the first N matches in walker order. Combined with
  `max_count` it gives you paging: `offset=20, max_count=20` returns the next
  page.
- `max_count_per_file` caps matches per file *before* the global budget is
  applied — it keeps one hot file from exhausting `max_count` before other
  files are reached.

## Type and glob filters

- `glob` compiles through [`try_compile_glob`](glob_util.md) (recursive
  prefix applies: `"*.rs"` matches `.rs` files at any depth).
- `r#type` maps well-known names to extension sets: `"js"`/`"javascript"` →
  `js, jsx, mjs, cjs`; `"ts"`/`"typescript"` → `ts, tsx, mts, cts`; `"py"` →
  `py, pyi`; `"rs"` → `rs`; `"docker"`/`"dockerfile"` → the `dockerfile`
  *filename*; `"make"`/`"makefile"` → the `makefile` filename; and so on. Any
  unknown value is treated as a **custom extension** (a leading `.` is
  stripped, case-insensitive).

## Pattern tolerances

Two niceties for callers who pass *literal text fragments* instead of
carefully-written regexes:

- **Braces that aren't valid repetition quantifiers are escaped** — a pattern
  like `${platform}` would fail to compile (`{p` is not a quantifier); it is
  silently rewritten to `$\{platform\}`. Valid quantifiers (`a{2,4}`) and
  Unicode property escapes (`\p{Greek}`, `\x{41}`) pass through unchanged.
- **After a group-syntax error, unescaped parentheses are escaped** — a
  trailing `(` from a snippet like `fetchAnthropicProvider(` is treated as a
  literal, retrying with `fetchAnthropicProvider\(`.

## The result

```rust,ignore
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,     // path, line_number, line, context, truncated, match_count
    pub total_matches: u32,          // across all files
    pub files_with_matches: u32,
    pub files_searched: u32,
    pub limit_reached: Option<bool>,
    pub skipped_oversized: Option<u32>,  // files > 4 MiB skipped
    pub timed_out: bool,             // partial results — search was cut short
}
```

Paths are **absolute** for single-file searches and **relative to the
searched directory** for directory searches. Files larger than 4 MiB are
skipped and counted in `skipped_oversized` rather than silently missing.

## Errors

| Situation | What happens |
|---|---|
| `path` missing | `Path not found: ...` |
| Invalid regex | `Regex error: ...` (after tolerances are applied) |
| Regex engine fails mid-search | `Search failed: ...` |
| Timeout before the walk starts | `Timeout` (hard error) |
| Mid-walk timeout | Partial `GrepResult` with `timed_out: true` |
| `path` is a special file (FIFO, socket) | empty `GrepResult` (no error) |

---

Next: [fs_cache — the shared scan cache](fs_cache.md), or [task —
cancellation](task.md).
