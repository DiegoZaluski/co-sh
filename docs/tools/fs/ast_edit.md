# `ast_edit` — the AST structural engine

`ast_edit` is the *structural* editing strategy of the [`fs` module](fs.md) —
the second engine behind `Fs::edit`, alongside the
[hashline replace engine](edit.md). Where `edit` anchors on line numbers and
content hashes, `ast_edit` rewrites the file's **syntax tree** directly: it
matches a code *pattern* structurally and replaces each match with a template.

```
Fs::edit(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String>   // with the `ast` argument
ast_edit(metadata, FsAstEdit) -> Result<Vec<EditResult>, String>               // the underlying free function
```

Use the AST engine when your change is *structural* — renaming a function's
signature across many call sites, wrapping every argument list in a helper,
updating a repeated idiom — rather than a literal text swap. For plain text
replacements, prefer the hashline engine.

---

## The model: pattern → template

Each rewrite operation is an [`AstEditOp`](types.md#astediop-and-fsastedi--the-ast-engine-edit-input):
a `pat` (pattern) and an `out` (replacement template).

1. **Parse.** `pat` is parsed as source code in the file's language. A pattern
   that does not parse is reported as a per-file warning; the op is skipped for
   that file rather than aborting the whole run.
2. **Match.** The pattern is matched *structurally* against the file's
   tree-sitter syntax tree. Matching is about shape, not text: whitespace and
   formatting differences do not break a match.
3. **Rewrite.** Each matched subtree's source text is replaced by `out`.
   Metavariables captured in `pat` are substituted into `out`.
4. **Repeat.** Multiple ops are applied in order, and for Rust, ast-grep also
   derives a statement-level contextual pattern so top-level and nested
   statements both match. The tree is re-parsed between derived patterns.

The whole set of matched files is gathered first, sorted, and deduplicated —
**order of the caller's `paths` list does not carry intention** here, unlike the
replace engine.

### Metavariables

Patterns support ast-grep metavariables:

| Metavariable | Matches |
|---|---|
| `$NAME` | Exactly **one** node. |
| `$$$NAME` | **Zero or more** nodes (for example, an argument list). |

Metavariable identity is enforced: the same metavariable must capture identical
text in every occurrence for a match to succeed.

Critical gotcha: **a metavariable captures a whole node — it never matches a
substring inside a literal.** To change the text of a string, match the whole
literal including its quotes:

```text
pat: console.log("$M")        // ✅ matches, captures the whole string node
out: console.log("[$M]")

pat: Hello, $X!               // ❌ a bare phrase is not a structurally valid snippet
```

Likewise, string literal patterns must include their quotes: `"$M"`, not `$M`.
Patterns must be **complete, structurally valid source snippets** for the
target language.

---

## Resolving which files to rewrite

The `paths` argument accepts any mix of:

- **Files** — explicit paths, rewritten if the language is supported.
- **Directories** — walked recursively; every file with a supported grammar is
  included. Symlinks are never followed (they could escape the root or form a
  cycle), and VCS/vendored subtrees (`.git`, `node_modules`) are skipped.
- **Globs** — expanded with gitignore-awareness, then validated.

Every resolved path is validated against the path guards and must have a
supporting tree-sitter grammar. Nonexistent explicit paths are silently skipped.
The result is **sorted and deduplicated**, then capped at `max_files`
(`DEFAULT_MAX_FILES`, 1000);
if the cap is reached, a note is appended warning you to narrow `paths`.

As with `write`/`edit`, a file that declares itself machine-generated is never
rewritten.

---

## What you get back

The result is one [`EditResult`](types.md#editresult) per file that was actually
modified by at least one rewrite, plus files that produced warnings (a pattern
failed to parse, or no match was found) so you see the feedback instead of
silence. Searched files with no changes and no warnings are not reported.

The shape is identical to the replace engine's output: fresh hash tag, the
1-based `first_changed_line`, warnings, and a unified `diff` when the file
changed.

### Errors

Unlike the replace engine, `ast_edit` returns a plain error string. A failure
names the failing file and aborts the run — there is no caller-intended order to
preserve, so no `applied`/`skipped` distinction. Top-level errors occur when:

- `paths` is empty, or `ops` is empty.
- A `pat` is empty or duplicated.
- A path is denied by the guards, a glob fails, or a file cannot be read or
  written back.

---

## Example

A complete runnable example is provided at
[`examples/fs/ast_edit.rs`](../../../crates/cosh-tools/examples/fs/ast_edit.rs).
It writes a small source file, then uses `Fs::edit` with an `ast` argument to
rename a function everywhere it appears — including across a directory tree.

Next: [rollback — restoring previous versions](rollback.md).