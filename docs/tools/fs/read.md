# `read` — reading files, symbols, and line ranges

`read` displays the contents of one or more files for the model. It is the
foundation of the whole [`fs` module](fs.md): everything you do — editing,
rolling back — begins by reading a file and copying the hashline tag and line
numbers from its output.

```
Fs::read(&self, targets: Vec<Target>) -> Vec<ReadResult>
```

It is the only file-system operation that never fails as a whole: **errors are
reported per-target inside each `ReadResult.warnings`**, and the rest of the
batch still runs. A missing file, an unresolvable block, or an invalid range
produces a warning, not an aborted batch.

This page walks through the four ways to read, the token-saving behaviors, and
what the output looks like.

---

## The four kinds of read

A [`Target`](types.md#target--one-read-request) selects *how much* of a file to
show. Remember the precedence rules:

1. `symbol` — a definition by name, checked first, wins over everything.
2. `line_range` — exact lines, plain slice, checked before `line`.
3. `line` — the syntactic block containing a line.
4. neither — the whole file.

### 1. Whole-file read

The simplest read: just a `path`, nothing else. The entire file is returned with
every line numbered.

```rust,ignore
let results = fs
    .read(vec![Target { path: "src/main.rs".into(), line: None, symbol: None, line_range: None }])
    .await;
```

Output (abridged):

```text
¶src/main.rs#3C4D
1| use clap::Parser;
2| use serde_json::Value;
3|
...
```

The `¶src/main.rs#3C4D` header is your anchor: copy the tag `3C4D` when you edit
the file later. Whole-file reads record every line as "seen", so later edits
anchoring any of those lines are safe.

### 2. Read by line — a syntactic block

Set `line` to a 1-based line number. The reader does **not** return just that
line: it uses tree-sitter to resolve the *syntactic block containing it* — the
enclosing function, struct, impl, enum, trait, or module — and returns the whole
block. This is the right tool when you want, say, "the function around line 40".

```rust,ignore
Target { path: "src/main.rs".into(), line: Some(40), symbol: None, line_range: None }
```

If the line does not begin a valid block, or the file has no grammar for the
line to resolve against, the read falls back to the whole file and reports a
warning explaining the likely causes.

### 3. Read by symbol — a definition by name

Set `symbol` to the name of a function, struct, class, or variable. The reader
searches the file for the definition and returns its block. When `path` is a
**directory**, the whole tree is walked recursively and every supported source
file is searched:

```rust,ignore
// Search src/greeter.rs for a definition named `greet`.
Target { path: "src/greeter.rs".into(), line: None, symbol: Some("greet".into()), line_range: None }

// Search every source file under src/ for a definition named `main`.
Target { path: "src".into(), line: None, symbol: Some("main".into()), line_range: None }
```

A directory read without a `symbol` is an error — the tool refuses to dump an
entire tree. If the symbol is not found anywhere, one result is returned with a
warning telling you to verify the spelling.

### 4. Read by `line_range` — exact lines, no AST

Sometimes you already know precisely which lines you need — for example, a block
was elided in a previous read and the footer told you to re-read lines `30-80`.
`line_range` gives you a plain slice with **no AST resolution**:

```rust,ignore
// Lines 50 through 100.
Target { path: "src/main.rs".into(), line: None, symbol: None, line_range: Some("50-100".into()) }

// Two disjoint ranges in one read.
Target { path: "src/main.rs".into(), line: None, symbol: None, line_range: Some("10-20,200-220".into()) }
```

Ranges are 1-based and inclusive. A `line_range` past the end of the file is
clamped, with a warning if it starts beyond EOF.

---

## What the output looks like

Every result is one [`ReadResult`](types.md#readresult): a `path`, the content
hash `file_hash`, the hashline `header`, the numbered `content`, and optional
`warnings`. The `content` is composed of three parts:

1. **The hashline header** — `¶path#TAG`, the anchor for edits.
2. **Numbered lines** — `N| text`, 1-based, the numbers you reference in edits.
3. **Content notices** — appended after the body, informing the model about
   elision and how to continue. These are *not* warnings; warnings are reserved
   for delivery problems (missing file, bad range, symbol not found).

### Token-saving behaviors

`read` is designed to keep the model's context lean. When you read less than a
whole file, three token-saving behaviors kick in automatically:

- **Elided blocks.** A block (from `line`/`symbol` resolution) larger than 24
  lines is shown as its head (3 lines), a `…` marker, and its tail (2 lines),
  with a footer naming the exact `line_range` to re-read for the elided body:

  ```text
  […78 lines elided; re-read with line_range "31-108"]
  ```

- **Column truncation.** Lines longer than 200 characters are cut with a `...`
  suffix and a notice telling you to use `line_range` for the full content.
  Truncation is flagged in the output so you know the line is incomplete.

- **Exact-range reads.** Reading `line_range` returns *only* the requested
  lines, with a footer reporting how many lines remain and how to continue:

  ```text
  [240 more lines in file; continue with line_range "61-300"]
  ```

  Between multiple ranges, gaps are marked with a summary line
  (`[…50 lines between ranges elided]`).

This "read only what you need" loop is the intended usage pattern: read a
block, notice it is elided, re-read the exact range, edit precisely.

### Seen-line tracking

Every line the read surfaces is recorded against the file's hash tag in the
session's rollback history. This matters for safety: if an edit later anchors a
line the model never actually saw — for example an elided interior — the
recovery path can warn instead of silently applying an edit against an unknown
body.

---

## Errors and edge cases

Because errors are per-target, there is no top-level error type for `read`.
Instead, look at `warnings`:

| Situation | What happens |
|---|---|
| File does not exist / cannot be read | `ReadResult` with a warning; empty content. |
| `path` is a directory without a `symbol` | A result explaining that a `symbol` is required. |
| Symbol not found | A result with a hint to verify spelling or use `line`. |
| No grammar for the language | A warning; use `line` targeting or read the whole file. |
| Invalid `line_range` syntax | A warning plus the whole file fallback. |
| `line` does not start a valid block | A warning plus the whole file fallback. |
| Path denied by the guards | A `permission denied` warning; the file is not read. |

In short: `read` always tells you *something*, and `warnings` is where you look
when a result's `content` is missing or surprising.

---

## Example

A complete runnable example is provided at
[`examples/fs/read.rs`](../../../crates/cosh-tools/examples/fs/read.rs). It
creates a scratch project, writes a small source file, and demonstrates all four
read modes — whole file, line block, symbol, and exact ranges.

Next: [write — creating and overwriting files](write.md).