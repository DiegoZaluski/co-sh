# `fs` data types: inputs and outputs

Every operation in the [`fs` module](fs.md) is driven by plain, `serde`-derived
structs that double as the JSON schema for the MCP tool arguments. This page is
the reference for those types and for the *hashline* output format they share.
It is organized in three parts:

1. The hashline format — the common output convention you must understand first.
2. Request types — what you pass **in** to `read`, `write`, and `edit`.
3. Response types — what you get **back**.

All the request types derive [`Deserialize`](https://docs.rs/serde/latest/serde/trait.Deserialize.html)
and `JsonSchema`, so they serialize directly to/from the tool-call JSON that an
agent sends and receives.

---

## The hashline output format

Reading, writing, editing, and rolling back all communicate through a shared
text convention called *hashline*. Understanding it is the key to using the
module well, so here is what you need to know.

Every operation that reports a file's state returns two fields: a `file_hash`
and a `header`. They are produced from the *whole file's content*:

- **`file_hash`** — a 4-hex-character fingerprint of the file's normalized text
  (uppercase, e.g. `1A2B`). It is content-derived: any read of byte-identical
  content mints the same tag, so two reads of an unchanged file agree.
- **`header`** — the display form, `¶path#TAG`. The `¶` prefix is the hashline
  file marker and `#` separates the path from the hash tag, e.g.
  `¶src/main.rs#3C4D`.

The hash is the anchor for safe editing. When you later call `edit`, you pass
the tag back; the engine refuses to apply your edit if the live file no longer
hashes to it (and attempts a 3-way merge recovery if it changed since you read
it). This is what prevents an agent from silently editing a file that moved
underneath it.

**Numbered lines.** When `read` displays file content it prefixes each line with
its 1-based line number using the `| ` separator: `N| text`. So line 1 of a file
containing `fn main() {}` renders as `1| fn main() {}`. These line numbers are
what you reference in edit operations (`replace 5..7:`, `insert after 15:`, ...).

The hashline tags are also recorded in the session rollback history every time a
file is read or written, which is what makes [`rollback`](rollback.md) possible.

---

## Request types

### `Target` — one read request

`Target` describes a single read. The only required field is `path`; the rest
select *how much* of the file you want, and they compose:

| Field | Type | Meaning |
|---|---|---|
| `path` | `String` | Path to read, relative to the project root (or absolute, if within the guards). |
| `line` | `Option<usize>` | A 1-based line number. When set, the reader resolves the *syntactic block* containing that line (function, struct, impl, ...) and returns it. |
| `symbol` | `Option<String>` | A symbol name (function, class, variable) to look up within the file, or across a directory tree. |
| `line_range` | `Option<String>` | An exact 1-based inclusive range or comma-separated list of ranges, e.g. `"50-100"` or `"10-20,200-220"`. A plain line slice — **no AST resolution**. |

Precedence and interaction rules, straight from the implementation:

- `symbol` is checked **first** and wins over everything: when set, the reader
  searches for the definition and ignores `line` and `line_range`.
- `line_range` is checked *before* `line` — if both are set, the exact slice
  wins over the AST block resolution.
- When `path` is a **directory**, a `symbol` *must* be provided; the reader then
  walks the tree recursively and searches every file with a supported
  tree-sitter grammar. Reading a directory without a symbol is an error.
- `path` without `line`/`symbol`/`line_range` reads the **whole file**.

> Reading less than a whole file is strongly encouraged. Block and symbol reads
> elide large blocks, exact-range reads truncate over-long columns, and both
> report how to continue, so the model reads only the lines it needs instead of
> whole files.

### `FsRead` — the `read` input

```rust
pub struct FsRead {
    pub targets: Vec<Target>,
}
```

The wrapper for a read batch. `Fs::read` accepts the targets directly and builds
this internally; the free function `read(metadata, FsRead)` takes it explicitly.
Errors are *not* fatal: a missing file or a bad `line_range` is reported as a
warning inside the matching `ReadResult`, and the rest of the batch still runs.

### `TargetFile` and `FsWrite` — the `write` input

```rust
pub struct TargetFile {
    pub text: String,
    pub path: String,
}

pub struct FsWrite {
    pub targets: Vec<TargetFile>,
}
```

One `TargetFile` is a complete overwrite: the file at `path` is replaced by
`text` in its entirety. There is no partial write — use [`edit`](edit.md) for
that. The text is normalized before writing (see [`write`](write.md)) and may
carry legacy hashline display prefixes copied from output; `N:` (colon) line
prefixes and `[path#hash]` headers are stripped automatically when the whole
content is consistently prefixed. The current `read` format — `¶path#TAG`
headers and `N| ` pipe prefixes — is **not** auto-stripped, so send the raw
file text without line-number prefixes.

### `EditTarget` and `FsEdit` — the replace-engine `edit` input

```rust
pub struct EditTarget {
    pub path: String,       // path to edit, relative to the project root
    pub file_hash: String,  // 4-hex tag from a read/search, e.g. "1A2B"
    pub ops: String,        // hashline edit operations, one per line
}

pub struct FsEdit {
    pub targets: Vec<EditTarget>,
    pub dry_run: bool, // preview: in-memory only, nothing is written
}
```

`file_hash` must match the file's *current* content (copy it verbatim from the
`¶path#TAG` header of a read). `ops` is a small edit DSL described on the
[edit page](edit.md); briefly:

```text
replace 5..7:
+fn hello() {
+    println!("hi");
+}
delete 10..12
insert after 15:
+// new comment
```

Order carries intention: targets are applied in the order listed, and a batch
aborts at the first failure (see [`EditBatchError`](#editbatcherror)).

### `ReplaceEdit` and `FsContentEdit` — the content-engine `edit` input

```rust
pub struct ReplaceEdit {
    pub path: String,               // path to edit, relative to the project root
    pub file_hash: Option<String>,  // 4-hex tag; required for the first edit of each file
    pub old_string: String,         // exact text to replace (unique unless replace_all)
    pub new_string: String,         // replacement text; empty deletes the match
    pub replace_all: bool,          // replace every occurrence (default false)
}

pub struct FsContentEdit {
    pub edits: Vec<ReplaceEdit>,
    pub dry_run: bool, // preview: in-memory only, nothing is written
}
```

`old_string` is a content address: it must match the tagged snapshot verbatim
and uniquely (unless `replace_all`). The edit is translated into the same
hashline ops as [`EditTarget`](#edittarget-and-fsedit--the-replace-engine-edit-input),
so tags, recovery, and diffs behave identically. See
[the `edits` argument](edit.md#the-edits-argument--the-content-replace-engine).

### `AstEditOp` and `FsAstEdit` — the AST-engine `edit` input

```rust
pub struct AstEditOp {
    pub pat: String,  // structural pattern to match
    pub out: String,  // replacement template
}

pub struct FsAstEdit {
    pub ops: Vec<AstEditOp>,   // rewrite ops, applied in order per file
    pub paths: Vec<String>,    // files, directories, or globs to rewrite
    pub max_files: Option<usize>, // optional cap (default: 1000)
}
```

Patterns are ast-grep style and structurally validated. `$NAME` matches exactly
one node; `$$$NAME` matches zero or more nodes (an argument list, for example).
The same metavariable must capture identical text in every occurrence for a
match to succeed. Full details and examples are on the
[AST engine page](ast_edit.md).

### `FsMetadata` — the shared configuration

```rust
pub struct FsMetadata {
    pub root: PathBuf,
    pub allowlist: Option<Vec<PathBuf>>,
    pub blocklist: Option<Vec<PathBuf>>,
}
```

The snapshot of the guards that the free functions receive. The `Fs` wrapper
builds this from its own state on every call — the write scope comes from
`allowlist`/`blocklist`, and the read scope substitutes
`read_allowlist`/`read_blocklist` when present. You normally never construct it
yourself. Each operation validates paths against it through the internal
`fs_guard` method (which delegates to `PathGuard`, see the
[permission model](fs.md#the-path-guard-permission-model)).

### `FsRollbackInput` and `FsRollback` — the `rollback` input

```rust
pub struct FsRollbackInput {
    pub path: String,
    pub hash: String,
}

pub struct FsRollback;  // marker type, currently unused
```

`FsRollbackInput` is the JSON shape an agent sends for the `fs_rollback` tool;
`Fs::rollback` receives `path` and `hash` as separate arguments instead. An
empty `hash` means *"restore the version immediately before the current file
content"*.

---

## Response types

Every response type implements `Serialize` except `EditBatchError` (an error
type, `Display`-formatted only), so the results serialize back to JSON for the
agent. `serde` also skips empty `Option` fields where noted.

### `ReadResult`

```rust
pub struct ReadResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,     // "¶path#TAG"
    pub content: String,    // numbered lines, possibly elided/truncated
    pub warnings: Option<String>,
}
```

One entry per successfully rendered target. `content` is what the model reads:
hashline header, then `N| text` lines, then any content notices (elision
markers, truncation notices, "how to continue" hints). `warnings` is `None` on
success and a human-readable explanation on partial or failed reads — a missing
file, an unresolvable block, a symbol not found, or an invalid `line_range`.
`ReadResult` does not carry a `first_changed_line`; see `EditResult` for that.

### `WriteResult`

```rust
pub struct WriteResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,     // "¶path#TAG"
    pub warnings: Option<String>,
}
```

Same shape as `ReadResult` minus `content`. On success, `file_hash`/`header`
describe the content that was just written, so a follow-up `edit` can anchor on
them directly. `warnings` reports non-fatal events such as auto-stripped
hashline prefixes, chmod +x on a shebang file, an empty file, a machine-generated
file that was refused, or a failed write.

### `EditResult`

```rust
pub struct EditResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,             // "¶path#TAG"
    pub first_changed_line: Option<u32>,  // 1-based, first line the edit touched
    pub warnings: Vec<String>,
    pub diff: Option<String>,       // unified diff when the file changed
}
```

Produced by both edit engines. `first_changed_line` is `None` when nothing
changed; `diff` is a 3-context-line unified diff between the before and after
text, serialized only when present. `warnings` collects per-file issues such as
an unparseable pattern or "no matches found".

### `EditBatchError`

```rust
pub struct EditBatchError {
    pub failed_path: String,      // the target that failed
    pub cause: String,            // the underlying error
    pub applied: Vec<EditResult>, // committed results before the failure
    pub skipped: Vec<String>,     // paths never attempted, after the failure
}
```

The error type of the replace engine's batch. As covered on the
[module overview](fs.md), when target N fails the batch stops: earlier results
stay committed (`applied`), and later targets are skipped as a consequence
(`skipped`), never reported as independent failures. It implements `Display`
with a readable multi-line message listing the fresh hash tags of the applied
results so the agent can continue editing those files without re-reading.

### `RollbackResult`

```rust
pub struct RollbackResult {
    pub path: String,
    pub file_hash: String,        // hash of the restored version now on disk
    pub header: String,           // "¶path#TAG"
    pub replaced_hash: String,    // hash of the previous disk state
    pub warning: Option<String>,
}
```

Returned by a successful [`rollback`](rollback.md). `replaced_hash` is the hash
of whatever was on disk *before* the restore; pass it back to a subsequent
rollback to undo the restore. `warning` notes anomalies that did not prevent the
restore (for example, the file was modified externally since the snapshot).