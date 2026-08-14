# `ast::ops` — compile, match, rewrite

The workhorse module. Everything here is a plain function over
[`SupportLang`](lang.md) and ast-grep's `Pattern` type.

---

## Errors

```rust
pub enum AstError {
    UnsupportedLanguage(String, String),  // unsupported + the supported list
    LanguageInference(String),            // can't infer from extension
    InvalidPattern(String),
    OverlappingReplacements,              // ambiguous edits
    EditRangeOutOfBounds,
    InvalidUtf8(String),
    Message(String),                      // free-form
}
```

All operations return `Result<_, AstError>` (`AstError` is also
`From<String>`, so internal messages flow through `Message`).

## Language resolution

```rust,ignore
pub fn resolve_supported_lang(value: &str) -> Result<SupportLang>          // alias → lang
pub fn resolve_language(lang: Option<&str>, file_path: &Path) -> Result<SupportLang>
pub fn is_supported_file(file_path: &Path, explicit_lang: Option<&str>) -> bool
pub fn supported_lang_list() -> String                                     // for error messages
```

`resolve_language` prefers an explicit `lang` (trimmed; empty counts as
absent) and falls back to extension inference. `is_supported_file` returns
`true` whenever an explicit language is given — the caller is asserting the
language, so any file is "supported".

## Pattern compilation

```rust,ignore
pub fn compile_pattern(
    pattern: &str,
    selector: Option<&str>,        // contextual selector, e.g. "expression"
    strictness: &MatchStrictness,
    lang: SupportLang,
) -> Result<Pattern>
```

- `selector: None` → compile the pattern as a standalone snippet.
- `selector: Some(kind)` → `Pattern::contextual` — the pattern is matched
  only in contexts of that node kind.
- **Multi-node fragments auto-wrap.** A fragment like `"key": $V` (JSON)
  parses to multiple root nodes and would fail as `MultipleNode`; it is
  transparently wrapped in a minimal valid context (`{"key": $V}`) and the
  pattern is compiled against the wrapping node kind. Only JSON has a
  wrapper template today; other languages keep the original error.

## Strictness

```rust
pub enum AstMatchStrictness { Cst, Smart, Ast, Relaxed, Signature, Template }
```

Maps onto ast-grep's `MatchStrictness`; `resolve_strictness(None)` yields
`Smart`, the default. Roughly:

| Level | Behavior |
|---|---|
| `Cst` | Exact concrete syntax — even trivia must match |
| `Smart` (default) | Ignores trivia differences; the standard "what you mean" mode |
| `Ast` / `Relaxed` / `Signature` / `Template` | Progressively looser (ast-grep semantics) |

## Searching

```rust,ignore
pub fn compile_search_patterns(pattern: &str, language: SupportLang) -> Result<Vec<Pattern>, PatternError>
pub fn collect_matches(source: &str, language: SupportLang, patterns: &[Pattern]) -> Vec<AstMatch>
```

`compile_search_patterns` returns **one or more** patterns:

- The compiled pattern itself, plus
- **Rust only**: a second *contextual* pattern (`expression_statement`
  selector), so a bare statement like `foo($A);` also matches inside
  function bodies. Other languages compile exactly one.

`collect_matches` runs every pattern over the source and returns all hits as
`AstMatch { line, column, end_line, end_column, byte_start, byte_end, text }`
(1-indexed line/column, byte offsets for spans).

## Rewriting

```rust,ignore
pub fn compile_rewrite_rules(rules: &[(String, String)], language: SupportLang)
    -> Result<Vec<CompiledRewrite>, (usize, PatternError)>   // index of the failing rule

pub fn rewrite_source(source: &str, language: SupportLang, ops: &[CompiledRewrite])
    -> Result<(String, u32), String>                          // (new text, replacement count)
```

`compile_rewrite_rules` compiles each `(pattern, replacement)` pair; the
error carries the **index** of the rule that failed. `rewrite_source`
applies every rule in order — each pattern's `replace_all` — re-parsing the
source after each rule so later rules see the updated tree. Returns the
final text and the total replacement count.

> **Order matters.** Rules run sequentially against the *current* text, so a
> rule can match nodes a previous rule created.

## Applying raw edits

```rust,ignore
pub fn apply_edits(content: &str, edits: &[Edit<String>]) -> Result<String>
```

The low-level applicator used by `rewrite_source`. Edits are sorted by
position, **deduplicated** (byte-identical edits collapse — multiple
patterns matching the same node are one deterministic edit), and checked for
overlaps:

- Overlapping divergent edits → `OverlappingReplacements`.
- Range past the end of the content → `EditRangeOutOfBounds`.
- Replacement not valid UTF-8 → `InvalidUtf8`.

## Discovering files

```rust,ignore
pub fn collect_matched_files(cwd: &Path, patterns: &[String]) -> Result<Vec<MatchedFile>, std::io::Error>
```

Walks `cwd` (respecting `.gitignore`, hidden included) and returns the files
matching the given glob patterns (or exact relative paths). `has_glob_syntax`
detects whether a pattern is a glob (`*`, `?`, `[`) vs. a literal path.

---

## Example

```rust,ignore
use cosh_sdk::ast::{
    SupportLang, compile_pattern, collect_matches, resolve_language,
    AstMatchStrictness,
};

let lang = resolve_language(None, std::path::Path::new("demo.rs")).unwrap();
let pattern = compile_pattern("console.log($MSG)", None, &AstMatchStrictness::Smart.into(), lang).unwrap();
let matches = collect_matches("console.log(1);\nconsole.log(\"hi\");\nother();", lang, &[pattern]);
assert_eq!(matches.len(), 2);
```

A complete runnable version lives at
[`examples/ast/ops.rs`](../../../crates/cosh-sdk/examples/ast/ops.rs).
