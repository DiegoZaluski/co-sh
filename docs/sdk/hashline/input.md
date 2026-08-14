# The hashline syntax: tokens, ops, and error handling

This page is the reference for the patch language itself — what a valid
patch looks like, how input is split into sections, and every diagnostic the
parser can produce. The two modules behind it are `tokenizer`
(line classification) and `parser` (the state machine that
turns tokens into [`Edit`](types.md)s); the high-level entry point is
[`Patch::parse`](input.md).

---

## Section headers

A patch is a sequence of **sections**, each rooted at a header:

```text
¶src/lib.rs#0A3
replace 5..7:
+fn double(x: i32) -> i32 {
+    x * 2
+}

¶src/main.rs#9F3E
insert tail:
+    println!("doubled: {n}");
```

- The header is `¶` + a path with **no embedded whitespace** + `#` + a
  **4-hex** uppercase content tag, e.g. `¶src/lib.rs#0A3F`. The tag is
  produced by [`compute_file_hash`](format.md), which always returns 4 hex
  chars.
- The strict tokenizer only recognizes a `#`-tag when it is exactly 4 hex
  chars (`#ABC` or `#ABCDE` do not parse as a tag — the trailing chars stay
  part of the path in the lenient recovery path, so a malformed tag is
  silently treated as a path rather than rejected).
- Quoted paths (`¶"src/lib.rs"#0A3`) are unquoted. A leading `***` and
  apply_patch-style noise (`Update File:`, `Update:`, `Add File:`, …) is
  stripped both from headers and from path text — the parser is built to
  forgive the shapes models reflexively emit.
- Malformed `¶`-prefixed lines are a hard error, not payload: `Input header
  must be ¶PATH or ¶PATH#TAG with a 4-hex content-hash tag; got "…".`
- Absolute paths in headers are normalized to cwd-relative when
  [`SplitOptions::cwd`](types.md) is set.

## Operations

Every op is a **verb header** followed by `+`-prefixed payload rows. The
grammar is strict about which ops take bodies:

| Op header | Body? | Notes |
|---|---|---|
| `replace 5..7:` / `replace 5:` | required | `EMPTY_REPLACE`: "`replace N..M:` needs at least one `+TEXT` body row. To delete lines, use `delete N..M`." |
| `delete 5..7` / `delete 5` | **none** | A colon or body is an error: "`delete N..M` does not take body rows. Remove the body, or use `replace N..M:`." |
| `insert before 5:` | required | `EMPTY_INSERT`: "`insert` needs at least one `+TEXT` body row." |
| `insert after 5:` | required | same |
| `insert head:` / `insert tail:` | required | same |
| `replace block 5:` | required | `EMPTY_BLOCK`: "`replace block N:` needs at least one `+TEXT` body row. To delete a block, use `delete N..M` with the block's line range." |
| `delete block 5` | **none** | "`delete block N` does not take body rows. Remove the body, or use `replace block N:` to replace the block." |

Range separators may be `..`, `-`, `…`, or `..` with spaces — the tokenizer
accepts all. A range that ends before it starts is an error (`line N: range
M..K ends before it starts.`). `delete` requires a range or single line
after the verb and rejects `delete N..M:` (colon) via a dedicated
diagnostic. `replace` accepts a single line (`replace 5:`).

### Payload rows

- Every literal row begins with `+`. Anything after the `+` is inserted
  verbatim (including leading whitespace and text that itself starts with
  `-`).
- A **bare row** (no `+`) inside a pending body is auto-converted to a
  literal row with a warning: `Auto-prefixed bare body row(s) with \`+\`.
  Body rows must be \`+TEXT\` literal lines; pasting raw code as payload is
  not a portable shape.` This is a warning, not an error — the content is
  preserved.
- A row starting with `-` (unified-diff style) is a hard error: `\`-\` rows
  are not valid; hashline ranges already name the lines being changed. To
  insert a literal line starting with \`-\`, write \`+-\`…`.` To author a
  literal line beginning with `-`, write `+-…`.
- Body rows with **no preceding op header** are an error: `line N: payload
  line has no preceding hunk header. Use \`replace N..M:\`, \`delete N..M\`,
  or \`insert before|after|head|tail:\` above the body. Got "…".`

### Overlapping deletes

Two hunks that both delete the same anchor line are rejected at parse end:
`line N: anchor line M is already targeted by another hunk on line K. Issue
ONE hunk per range; payload is only the final desired content, never a
before/after pair.`

## Contamination detection

The parser actively looks for **other patch formats** and refuses them with
a targeted message instead of misparsing them:

| Input shape | Diagnostic |
|---|---|
| `*** Update File:…` (apply_patch sentinel) | "apply_patch sentinel … is not valid in hashline. File sections start with `¶path#HASH` …" |
| `@@ -N,M +N,M @@` | "unified-diff hunk header … is not valid in hashline …" |
| `@@`-bracketed header (loose) | "`@@`-bracketed hunk header … is not valid in hashline. Drop the `@@ ... @@` brackets …" |
| `delete 5..7:` (colon) | "`delete N..M` has no colon and no body. Remove the colon and body rows." |
| `5` (bare number) | "hunk headers need a verb. Use `replace 5..5:` to replace, or `delete 5` to delete." |
| `5..7` (bare range) | "bare range hunk header … is not valid. Hunk headers need a verb: write `replace 5..7:` or `delete 5..7`." |

## Envelope markers and comments

- `*** Begin Patch` and `*** End Patch` are an optional envelope: the begin
  marker is silently consumed (even at the very start of input), the end
  marker **terminates parsing** — everything after it is ignored.
- `*** Abort` is a recovery sentinel emitted by an agent loop when a
  contaminated tool-call stream is truncated mid-call. It behaves like
  `*** End Patch` (terminates) but surfaces no warning.
- Lines beginning with `#` **between** hunks are treated as skippable
  comments and dropped. A `#` line inside a pending body is payload.

## Splitting and the `Patch`

[`Patch::parse(input, &SplitOptions)`](input.md) is the entry point. It:

1. Strips a leading BOM and blank lines.
2. **Requires** the first non-blank line to be a valid header. Otherwise it
   errors with guidance: `input must begin with "¶PATH#HASH" on the first
   non-blank line for anchored edits; got: "…". Example: "¶src/foo.ts#0A3"
   then edit ops.` A first line of `@@ … @@` gets the unified-diff-specific
   message. A `SplitOptions::path` fallback can inject a header when the
   input has recognizable ops but no header (`contains_recognizable_hashline_operations`
   decides), which is how streaming previews work before the model writes
   the header.
3. Splits on subsequent headers. Sections with **no ops** are dropped;
   consecutive sections for the **same path** are merged into one section
   with concatenated diffs (anchors authored against one snapshot must apply
   as one batch — applying them separately would shift line numbers).
   Merging with **conflicting hash tags** for the same path panics with
   `Conflicting hashline snapshot tags for {path}. Re-read the file and retry
   with one current header.` (This is the one assert in the split path —
   it indicates contradictory authoring input.)
4. Returns a [`Patch`](input.md) whose sections parse their diffs **lazily**
   on first access and cache the result, so preflight and apply never parse
   twice.

`Patch::parse_single` is the convenience for the one-section case: same
rules, returns the first section, errors if the input produced zero
sections.

## Streaming-tolerant parsing

`parse_patch_streaming` and
[`PatchSection::apply_partial_to`](input.md) are the streaming-tolerant
variants: a trailing in-flight op (no payload yet) or a per-token parse
error mid-stream becomes a **warning**, never a hard error or a phantom
empty-payload edit. The authoritative writer path should always use the
strict `parse_patch` / `apply_to`.
