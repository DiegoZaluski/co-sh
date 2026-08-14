# The `ast` module: structural search and rewrite

`ast` is the **AST-aware search and rewrite engine** powered by
[ast-grep](https://ast-grep.github.io/). Where [`tree_sitter`](../tree_sitter/tree_sitter.md)
gives you raw syntax trees, `ast` gives you the high-level operations on top:
compile a *pattern* (snippet with metavariables), find every node that
matches it, and rewrite those nodes in place.

```
pattern "console.log($MSG)"
        │  compile_pattern ──▶ Pattern (ast-grep)
        ▼
source  ──▶ collect_matches ──▶ Vec<AstMatch>        (find)
        ──▶ rewrite_source   ──▶ (String, u32)       (replace)
```

This is the engine behind cosh-tools' `fs_ast_edit` tool. It is vendored
from oh-my-pi's `pi-ast`/`pi-natives`, stripped of the N-API/JS surface.

---

## The three layers

| Layer | Module | What it provides |
|---|---|---|
| Languages | [`lang`](lang.md) | [`SupportLang`] — 60+ grammars, alias resolution, extension inference |
| Parsers | [`parse`](parse.md) | The tree-sitter parser function for every language |
| Operations | [`ops`](ops.md) | Pattern compilation, matching, rewriting, edit application, file discovery |

> See [Language resolution](#language-resolution) below for how to obtain a `SupportLang`, and [`ops.md`](ops.md#strictness) for matching behavior options.

The public entry points are re-exported from `ast::ops` (and `SupportLang`
from `ast::lang`), so `use cosh_sdk::ast::{SupportLang, collect_matches,
rewrite_source, …}` is all you typically need.

## The core concept: patterns with metavariables

An ast-grep pattern is a **code snippet** that may contain metavariables —
`$NAME` placeholders that match any single node:

```rust,ignore
// pattern                 matches
console.log($MSG)          every console.log(...) call
fn $NAME($PARAMS) $BODY    every function definition
$x + $x                    every expression adding a thing to itself
```

`$$$ARGS` (three dollars) matches *zero or more* nodes. Patterns are parsed
as real syntax in the target language, so they are structurally precise —
`foo($A)` matches `foo(x)` but not `foo(x, y)`.

Matching has a **strictness** knob (see [ops](ops.md)): `Smart` (default)
ignores trivia like whitespace and comment placement, `Cst` demands exact
concrete-syntax equality, and the others trade precision for recall.

## Language resolution

Operations take a [`SupportLang`], which callers obtain in one of two ways:

- **Explicitly** — `SupportLang::from_alias("py")` or `resolve_supported_lang("python")`.
- **From a file** — `SupportLang::from_path("src/main.rs")` / `resolve_language(None, path)`,
  which infer the language from the extension (or well-known filename:
  `Makefile`, `Justfile`, `CMakeLists.txt`, `Dockerfile`, shell rc files).

Unknown languages and uninferable paths are structured errors, not panics
([`AstError`](ops.md)).

---

## Example

A complete, runnable walkthrough lives at
[`examples/ast/ops.rs`](../../../crates/cosh-sdk/examples/ast/ops.rs): it
resolves languages, compiles patterns with metavariables, collects matches,
rewrites a snippet, applies raw edits, and shows the error paths.

---

## Summary

- `ast` is the AST-aware search and rewrite engine powered by ast-grep, providing high-level pattern compilation, matching, and rewriting operations.
- The module has three layers: Languages (SupportLang registry), Parsers (tree-sitter functions), and Operations (pattern compilation, matching, rewriting).
- Patterns use metavariables (`$NAME` for single nodes, `$$$ARGS` for zero or more) and are parsed as real syntax in the target language.
- Matching has a strictness knob: `Smart` (default, ignores trivia), `Cst` (exact concrete syntax), and progressively looser modes.
- Language resolution can be explicit (from alias) or inferred from file paths; unknown languages are structured errors, not panics.
- Public entry points are re-exported from `ast::ops` and `ast::lang`, so typical usage is `use cosh_sdk::ast::{SupportLang, collect_matches, rewrite_source, …}`.

Next: [language resolution](lang.md), then [the operations](ops.md).
