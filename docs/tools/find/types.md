# `find` data types: inputs and outputs

Both find tools are driven by plain, `serde`-derived structs that double as
the JSON schema for the MCP tool arguments. This page is the reference for
those types. It is organized in three parts:

1. Request types — what you pass **in** to `glob` and `grep`.
2. Config structs — the internal `Glob`/`Grep` option bundles.
3. Response types — what you get **back**.

---

## Request types

### `GlobInput` — the `find_glob` arguments

```rust
pub struct GlobInput {
    pub pattern: String,          // e.g. "**/*.rs", "src/**", "*.toml"
    pub path: Option<String>,     // a root, glob, or literal file/dir
    pub paths: Option<Vec<String>>, // multiple targets; overrides `path`
    pub file_type: Option<String>,  // "file" | "dir" | "symlink"
    pub recursive: Option<bool>,    // default: derived from pattern shape
    pub hidden: Option<bool>,       // effective default: true
    pub max_results: Option<u32>,   // default 200, ceiling 200
    pub gitignore: Option<bool>,    // effective default: true
    pub sort_by_mtime: Option<bool>,// default: true (most recent first)
    pub format: Option<String>,     // "flat" | "grouped" | "tree"
    pub timeout_ms: Option<u32>,    // default 5000; partials on expiry
}
```

`pattern` and `path`/`paths` are both optional in effect: when a target carries
its own glob characters (`src/*.rs`), the target's glob wins and `pattern` is
ignored. When the target is a plain directory, `pattern` applies within it.
See [glob](glob.md#how-targets-and-patterns-combine) for the full rules.

### `GrepInput` — the `find_grep` arguments

```rust
pub struct GrepInput {
    pub pattern: String,            // regex to match in file contents
    pub path: String,               // root directory to search
    pub paths: Option<Vec<String>>, // multiple targets; overrides `path`
    pub line_range: Option<String>, // "start-end", 1-based inclusive, single-file only
    pub glob: Option<String>,       // restrict by file-name glob, e.g. "*.rs"
    pub file_type: Option<String>,  // restrict by language, e.g. "rust"
    pub ignore_case: Option<bool>,  // default: false
    pub max_count: Option<u32>,     // match cap; disables file-window paging
    pub skip: Option<u32>,          // files to skip — page past the window
    pub context_before: Option<u32>,// context lines before each match
    pub context_after: Option<u32>, // context lines after each match
    pub hidden: Option<bool>,       // effective default: true
    pub gitignore: Option<bool>,    // effective default: true
    pub timeout_ms: Option<u32>,    // default 5000; partials on expiry
}
```

---

## Config structs

`Glob` and `Grep` are the internal option bundles the free functions accept.
They mirror the input fields above (minus `pattern`/`path`, which are separate
arguments) and implement `Default` so you can build them incrementally:

```rust
let glob = Glob { file_type: Some("dir".into()), max_results: Some(20), ..Default::default() };
cosh_tools::find::glob(&glob, "**", "src")?;
```

> **Caveat: the free-function structs are raw config.** Fields you leave `None`
> fall through to the SDK's own defaults, which differ from the `Find` wrapper's
> reference defaults for glob: `hidden` defaults to `false` and results are
> **uncapped** (no `max_results` ceiling, no `limit_reached`). The wrapper
> (`Find`) applies `hidden: true` and the `200`/`200` cap; prefer it unless you
> are deliberately tuning the raw engine.

`GlobCallOptions` is the schema-driven bundle for `Find::glob_full`, so the
call signature stays small as the schema grows. It carries `file_type`,
`hidden`, `gitignore`, `max_results`, `format`, `sort_by_mtime`, and
`timeout_ms` — each `Option`, falling back to the builder value, then to the
tool default.

---

## Response types

### `GlobEntry` and `GlobOutput`

```rust
pub struct GlobEntry {
    pub path: String,          // CWD-relative (or root-relative), `/`-separated
    pub file_type: String,     // "file" | "dir" | "symlink"
    pub mtime_ms: Option<f64>, // mtime as ms since the Unix epoch
    pub size_bytes: Option<f64>, // file size; None for dirs/symlinks
}

pub struct GlobOutput {
    pub matches: Vec<GlobEntry>,
    pub total: u32,
    pub limit_reached: Option<bool>, // cap cut the list; more may exist
    pub timed_out: Option<bool>,     // partial results; incomplete scan
    pub note: Option<String>,
    pub useless: Option<bool>,       // zero matches, no timeout
    pub missing_paths: Option<Vec<String>>, // skipped missing targets
    pub formatted: String,           // paths in flat/grouped/tree layout
    pub scope: String,               // directory the search was scoped to
    pub cwd: Option<String>,         // working dir paths are relative to
}
```

`formatted` is **always present**, even for empty results — the model never has
to reconstruct grouping from the structured entries. When no `format` was
requested it is the plain newline-joined paths. Directories are rendered with a
trailing `/`.

`scope` is the directory the search was scoped to, in the same relative form as
the match paths (`.`, `src/`, `crates/…`); `cwd` lets the TUI renderer resolve
match paths to absolute paths for file hyperlinks.

### `GrepMatchEntry`, `ContextEntry`, `GrepFileEntry`, `GrepOutput`

```rust
pub struct ContextEntry {
    pub line_number: u32,   // 1-indexed
    pub line: String,
}

pub struct GrepMatchEntry {
    pub path: String,       // absolute (single-file) or root/ancestor-relative
    pub line_number: u32,   // 1-indexed
    pub line: String,
    pub truncated: Option<bool>, // line was cut to the 200-char column limit
    pub context_before: Vec<ContextEntry>,
    pub context_after: Vec<ContextEntry>,
}

pub struct GrepFileEntry {
    pub path: String,       // same form as the match paths
    pub file_hash: String,  // 4-hex tag — the `file_hash` for `fs_edit`
    pub header: String,     // "¶abs/path#TAG" — the hashline anchor
}

pub struct GrepOutput {
    pub matches: Vec<GrepMatchEntry>,
    pub total_matches: u32,       // dedup per-line union (lower bound if capped)
    pub files_with_matches: u32,
    pub files_searched: u32,
    pub file_limit_reached: bool, // more files matched than the shown window
    pub per_file_limit_reached: bool, // a hot file exceeded its match cap
    pub note: Option<String>,
    pub useless: Option<bool>,
    pub files: Vec<GrepFileEntry>, // hashline anchors for shown files
    pub timed_out: Option<bool>,
}
```

`total_matches` is the deduplicated per-line union across targets *before* the
per-file caps and window trim (so two matches on the same line of the same file
count once; itself bounded by the engine's per-file fetch and per-target
ceiling) — a reliable lower bound even when matches were trimmed. With
`line_range`, it is the count of in-range matches.

The `files` array is the bridge to editing: each shown file within the anchor
window (the first 20 in encounter order) carries a `path`, a `file_hash`, and a
`header` like `¶/abs/path#TAG`. Pair a match's `path` with the matching
`file_hash` and pass both to `fs_edit` — no re-read needed. See
[grep](grep.md#editing-matches-directly).