# `edit` — the hashline replace engine

`edit` applies targeted changes to one or more files. It is the surgical
alternative to `write`: instead of replacing a whole file, it changes specific
lines or blocks, anchored by the file's content hash for safety.

```
Fs::edit(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String>
```

`Fs::edit` is a dispatcher: depending on the configured
[`EditEngine`](fs.md#the-two-edit-engines), the `args` it receives select the
engine to run. This page covers the **hashline replace engine** (the `targets`
argument, or the engine forced by `Fs::only_replace()`). The structural engine
is documented on its own page, [`ast_edit`](ast_edit.md).

---

## The idea: hash-anchored edits

Before any edit is applied, the engine re-hashes the file's *current* content
and compares it with the `file_hash` you passed in:

- **Hash matches** → the file is exactly what you read; the edit is safe.
- **Hash differs** → the file changed since you read it. The engine attempts an
  automatic **3-way merge recovery** (your edit against the live content), and
  only applies if the merge is unambiguous.

This is what prevents editing a stale view of a file. It is also why the
`file_hash` must be copied *verbatim* from the `¶path#TAG` header of a read or
search result — never re-typed.

Order carries intention. Targets are applied **in the order you list them**; an
edit may build on the result of a previous one. When a target fails, the batch
stops: earlier targets stay applied (returned with fresh hash tags), and later
targets are skipped as a consequence of the failure — see
[`EditBatchError`](types.md#editbatcherror).

---

## The `ops` mini-language

Each [`EditTarget`](types.md#edittarget-and-fsedit--the-replace-engine-edit-input)
carries an `ops` string: one operation per line. The operations are:

| Operation | Syntax | Effect |
|---|---|---|
| Replace | `replace N..M:` followed by `+`-prefixed lines | Replace lines N through M with the new content. |
| Replace one line | `replace N:` + `+line` | Replace just line N (lenient form of `N..N`). |
| Delete | `delete N` or `delete N..M` | Delete line N, or lines N through M. |
| Insert before | `insert before N:` + `+`-prefixed lines | Insert lines before line N. |
| Insert after | `insert after N:` + `+`-prefixed lines | Insert lines after line N. |
| Insert at head | `insert head:` + lines | Insert at the start of the file. |
| Insert at tail | `insert tail:` + lines | Insert at the end of the file. |
| Replace block | `replace block N:` + lines | Resolve the tree-sitter block starting at line N and replace it. |

Each payload line must begin with `+` (the payload sigil). Multi-line payloads
repeat the `+` prefix on every line. The parser is deliberately lenient about
separators (`..`, `-`, `…`, or spaces between range endpoints are all accepted;
a trailing `:` on `replace`/`insert` headers is optional — for `delete` it is an
error) but a line that starts with `+` when no body is expected is an error.

A complete `ops` string for a two-part edit:

```text
replace 5..7:
+fn hello() {
+    println!("hi");
+}
delete 10..12
insert after 15:
+// new comment
```

Line numbers refer to the file **as it was when you read it** — the hashline
numbers in the `N| text` output. The engine maps them through the content hash
anchor, so intervening edits to the file are detected rather than silently
mis-applied.

---

## `Fs::edit` in `Replace` mode

When the `Fs` is forced to the replace engine (`only_replace()`), `edit` expects
exactly one argument: a non-empty `targets` array.

```rust,ignore
let args = serde_json::json!({
    "targets": [{
        "path": "src/main.rs",
        "file_hash": "3C4D",          // copied from ¶src/main.rs#3C4D
        "ops": "replace 1..3:\n+fn main() {\n+    println!(\"hi\");\n+}"
    }]
});
let results = fs.edit(args).await?;
```

If `targets` is missing, `edit` returns an error explaining the expected shape.

### In `Auto` mode

The default `EditEngine::Auto` inspects the arguments and routes accordingly:
a non-empty `targets` array runs the replace engine; an `ast` object runs the
AST engine. Providing both or neither is an error, and filling `targets` with
AST-style metavariables (`$NAME`) is detected and corrected with a prompt
telling you the AST schema belongs in `ast`. See
[the dispatcher](fs.md#how-auto-dispatches) for the full picture.

---

## What you get back

Each applied target yields an [`EditResult`](types.md#editresult): the fresh
hash tag (your new anchor — keep editing without re-reading), the 1-based
`first_changed_line`, any `warnings`, and a unified `diff` of the change (only
when something actually changed). A target whose edit produced no change returns
a result with `diff: None`.

On batch failure, `Err` carries an [`EditBatchError`](types.md#editbatcherror)
describing which target failed, which were applied before it, and which were
skipped after.

---

## Example

A complete runnable example is provided at
[`examples/fs/edit.rs`](../../../crates/cosh-tools/examples/fs/edit.rs). It
writes a small source file, reads it back to capture its hashline tag, then
applies a multi-operation edit and prints the resulting diff.

Next: [ast_edit — the AST structural engine](ast_edit.md).

---

## Dry run — preview before applying

Both `targets` and `edits` accept `"dry_run": true` at the top level of the
arguments. The edit is then applied **in memory only**: the response carries
the unified `diff`, the fresh `¶path#TAG` header the real edit would produce,
and the syntax probe's verdict — including the
*"This edit introduced a syntax error near line N"* advisory when the result
would no longer parse. Nothing is written: no file changes, no rollback
record, no passive LSP pull. The result's warnings end with
*"Dry run: nothing was written — reissue without `dry_run` to apply"* and the
`dry_run: true` marker is set on the result so a preview can never be
mistaken for an applied edit.

The intended loop for risky line-range edits: preview first, read the diff +
advisory, fix the range if needed, then re-issue the same arguments without
`dry_run`.

---

## The `edits` argument — the content replace engine

`targets` addresses lines; **`edits` addresses content**. Each edit replaces an
exact `old_string` with `new_string`:

```rust,ignore
let args = serde_json::json!({
    "edits": [{
        "path": "src/main.rs",
        "file_hash": "3C4D",              // same ¶path#TAG anchor as targets
        "old_string": "old_computation(x)",
        "new_string": "new_computation(x)"
    }]
});
let results = fs.edit(args).await?;
```

Rules and behavior:

- **Exact, unique match.** `old_string` must match the file verbatim — all
  whitespace and newlines included — and must occur exactly once, unless
  `replace_all: true` is set. Use the smallest snippet that is unique; add
  surrounding context to disambiguate.
- **Still hash-anchored.** `file_hash` is required (for the first edit of each
  file in the call; follow-up edits to the same file in the same call may omit
  it and chain on the previous edit's fresh tag). The match runs against the
  tagged snapshot and the change is applied through the same pipeline as
  `targets`, so stale-hash detection, 3-way merge recovery, diffs, and passive
  LSP feedback behave identically.
- **Line-granular translation.** A partial-line match keeps its surrounding
  context (`prefix + replacement + suffix` on the affected lines); empty
  `new_string` deletes the matched text. After applying, the result is
  verified against the exact character-level replacement — if the hashline
  engine's boundary/indent repair would alter it, the edit is rejected
  (nothing is written) and the model is pointed at `targets`.
- **Line-terminator boundaries.** A match ending with a newline followed by
  `new_string` without one joins the replacement with the next line (exact
  character semantics); when there is no following line (the file's final
  newline), only a whole-line deletion is expressible — anything else is
  rejected with a diagnostic instead of silently dropping the trailing
  newline.
- **Rejections teach.** Zero matches run a fuzzy closest-match search over
  the file (whitespace/typo-tolerant similarity, ported from oh-my-pi) and
  report the nearest region: similarity %, line, and the first differing
  line as `-`/`+` — or the preview windows of every occurrence when the text
  is ambiguous. The fuzzy result is advisory only: it NEVER substitutes
  anything; the model re-issues with the corrected exact text.

Choose `edits` when the text itself identifies the location and computing line
numbers would be busywork; choose `targets` when you already know the line
positions, and `ast` for structural rewrites.