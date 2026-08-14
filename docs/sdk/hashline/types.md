# Hashline data types

The pipeline passes a handful of pure data types around. Everything here is
plain, `Clone`-able data — no filesystem, no runtime, no schema library.
This is the vocabulary the rest of the module speaks.

---

## `Anchor` and `ParsedRange` — line references

```rust
pub struct Anchor { pub line: u32 }                       // 1-indexed

pub struct ParsedRange {
    pub start: Anchor,
    pub end: Anchor,
}
```

Line numbers are **1-indexed and inclusive**. `replace 5..7:` names lines 5,
6, and 7. A range may be a single line (`replace 5:` → `start == end`).
Out-of-bounds anchors are caught at apply time (`Line N does not exist (file
has M lines)`), not at parse time — parsing is purely lexical.

## `Cursor` — where an insert lands

```rust
pub enum Cursor {
    Bof,                            // insert head:
    Eof,                            // insert tail:
    BeforeAnchor(Anchor),           // insert before 5:
    AfterAnchor(Anchor),            // insert after 5:
}
```

`Bof`/`Eof` are **position-stable**: "start of file" and "end of file"
cannot move with content drift, so `insert head:`/`insert tail:` edits are
safe to apply even when the section's hash tag is stale (the patcher applies
them with a warning instead of rejecting — see
`HEADTAIL_DRIFT_WARNING`). The anchor forms are the opposite:
they name a concrete line, so drift makes them suspect.

## `Edit` — one low-level operation

```rust
pub enum Edit {
    Insert {
        cursor: Cursor,
        text: String,
        line_num: u32,      // source line the op was authored on (diagnostics)
        index: u32,         // ordering bookkeeping
        mode: Option<Replacement>,
    },
    Delete {
        anchor: Anchor,
        line_num: u32,
        index: u32,
        old_assertion: Option<String>,   // reserved; always None today
    },
    Block {
        anchor: Anchor,
        payloads: Vec<String>,
        line_num: u32,
        index: u32,
    },
}
```

Edits are produced by the `parser` and consumed by the
[applier](apply.md). Three things worth knowing:

- **Multi-line replacements decompose.** `replace 5..7:` with three payload
  rows becomes three `Insert` (one per row, cursor `BeforeAnchor(5)`, mode
  `Replacement`) plus three `Delete` (lines 5, 6, 7). The `Replacement`
  marker lets the applier treat those inserts as *new content for a deleted
  line* rather than additions alongside it.
- **`Block` is deferred, never applied.** `replace block N:` doesn't know
  its line span until the file text and language are available, so the
  parser emits a `Block` edit. [`resolve_block_edits`](block.md) expands it
  into the same inserts + deletes that an explicit `replace start..end:`
  would produce. `apply_edits` panics if it ever sees a `Block` variant —
  that's a wiring bug (`internal error: unresolved \`replace block\` edit
  reached the applier`).
- **`index` is advisory.** The applier re-derives ordering from array order;
  the parser assigns increasing indices so diagnostics are stable.

## `ApplyResult` — what applying produced

```rust
pub struct ApplyResult {
    pub text: String,                    // post-edit text body
    pub first_changed_line: Option<u32>, // 1-indexed, None for a no-op
    pub warnings: Vec<String>,           // parser/patcher/recovery diagnostics
}
```

`apply_edits`, `PatchSection::apply_to`, and the recovery paths all return
this shape. `first_changed_line` is how a caller reports "what moved" in a
diff preview without diffing.

## `BlockSpan` and the resolver seam

```rust
pub struct BlockSpan { pub start: u32, pub end: u32 }   // 1-indexed, inclusive

pub struct BlockResolverRequest {
    pub path: String,   // language inferred from the extension
    pub text: String,   // full text to resolve against (the snapshot the tag names)
    pub line: u32,      // line the block must begin on
}

pub type BlockResolver = fn(BlockResolverRequest) -> Option<BlockSpan>;
```

The hashline core declares the contract — "given a file and a line, return
the syntactic block that begins there" — and lets the host inject the
implementation (a tree-sitter backed resolver, for instance). `None` means
"no block can be resolved" (unsupported language, blank/out-of-range line,
no node begins there, syntax error). See [block](block.md) for how the
result is used and what `Throw` vs `Drop` mean.

## `Replacement` / `ResolveAction` — small markers

```rust
pub enum Replacement { Replacement }        // tags replacement-mode inserts
pub enum ResolveAction { Throw, Drop }      // what to do with unresolvable blocks
```

`Throw` (the default) makes an unresolvable block a hard error — used by the
authoritative apply and final preview paths. `Drop` silently skips the edit —
used by streaming previews where a half-written file must not throw.

## `SplitOptions` / `StreamOptions` — input knobs

```rust
pub struct SplitOptions {
    pub cwd: Option<String>,  // resolve absolute header paths to cwd-relative
    pub path: Option<String>, // fallback path when input lacks a header
}

pub struct StreamOptions {
    pub start_line: Option<u32>,   // first line number, default 1
    pub max_chunk_lines: Option<u32>, // max lines per chunk, default 200
    pub max_chunk_bytes: Option<u32>, // max UTF-8 bytes per chunk, default 64 KiB
}
```

`SplitOptions` feeds [`Patch::parse`](input.md). `cwd` lets a patch authored
with absolute paths (the common case for model output) be stored with
cwd-relative paths; `path` provides a fallback header for input that has
recognizable ops but no `¶PATH` yet — useful for streaming previews before
the model writes the header. `StreamOptions` feeds
`stream_hash_lines`.

## `CompactDiffPreview` / `CompactDiffOptions`

```rust
pub struct CompactDiffPreview {
    pub preview: String,     // truncated, annotated preview text
    pub added_lines: u32,
    pub removed_lines: u32,
}
pub struct CompactDiffOptions { pub max_unchanged_run: Option<u32> }  // default 2
```

Produced by `build_compact_diff_preview` — a bounded
preview of the change with unchanged-run truncation.
