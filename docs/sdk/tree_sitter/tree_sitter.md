# The `tree_sitter` module: cached syntax trees

`tree_sitter` manages the **lifecycle and reuse of Tree-sitter syntax
trees** behind a small LRU cache. It answers two questions that the rest of
the crate needs answered constantly:

1. *What is the syntactic block on line N of this file?* — for
   `replace block N:` edits (see [`hashline::block`](../hashline/block.md)).
2. *Where does the definition named X live?* — for symbol-targeted reads.

And it makes sure answering them is cheap: parsing is done once per file
version, never per query.

```
file read  ──▶  TreeSitter::resolve_block / resolve_symbol / with_tree
                    │
                    ├─ cache miss  → detect language → full parse
                    ├─ cache hit (same text) → reuse tree, no re-parse
                    └─ cache hit (changed text) → incremental re-parse
```

---

## The lifecycle: parse once, reuse, update incrementally

The module's core promise is that a syntax tree is **reused across calls**
until the text actually changes:

- **Miss** — the path is new to the cache: detect the language from the
  extension and perform a full parse.
- **Hit, unchanged** — the text is byte-identical to the cached text: hand
  back the cached tree instantly, no re-parse.
- **Hit, changed** — the text differs: perform an **incremental parse**,
  reusing the old tree so only the changed regions are re-lexed.

Because `Tree` is not `Clone`, entries are popped from the cache for the
duration of each call and put back afterward — a `Mutex<LruCache>` guards
the pool.

## The global instance

```rust,ignore
pub fn tree_sitter() -> &'static TreeSitter;
```

A process-global singleton with a 128-entry LRU, initialized on first use.
This is what the file tools use: `rollback` and the cosh-tools file
operations call `tree_sitter().invalidate(path)` after writing, so a stale
tree is never served. Construct your own `TreeSitter::new(capacity)` when
you need an isolated pool (tests, sandboxes).

## The three entry points

| Method | Returns | Use for |
|---|---|---|
| [`resolve_block`](block.md) | `Option<BlockSpan>` | "the syntactic block beginning on line N" — the `replace block N:` resolver |
| [`resolve_symbol`](block.md) | `Option<BlockSpan>` | "the definition named X" — symbol-targeted reads |
| [`with_tree`](block.md) | `Option<R>` | Raw `&Tree` access for advanced consumers (AST walking, matching) |

All three return `None` when the language is unsupported or parsing fails,
and all three share the same cache lifecycle described above.
`parse_count()` reports how many real `parser.parse` calls have happened
(cache hits don't increment it) — handy for observing the lifecycle.

## Supported languages

Detection is by file extension (see [language](language.md) for the full
table — 60+ extensions across 40+ grammars). The doc comment's
`tree_sitter()` summary names the core set — Rust, Python, JavaScript, C#,
Go, Java, Haskell, Swift, Zig, Kotlin — but `detect_language` covers far
more, and `highlight` accepts an explicit language name.

## Relationship to the other crates

- **`hashline`** — `BlockSpan` comes from `hashline::types`; `resolve_block`
  is exactly the [`BlockResolver`](../hashline/block.md) contract the
  patcher accepts (cosh-tools wires it into `Patcher::new_shared`).
- **`ast`** — the ast-grep registry and `detect_language` mirror each
  other's extension sets; the low-level parse cache deliberately does not
  depend on the structural-matching engine.
- **`rollback`** — `restore` invalidates the cache so a rollback never
  serves a stale tree.

---

## Example

A complete, runnable walkthrough lives at
[`examples/tree_sitter/tree_sitter.rs`](../../../crates/cosh-sdk/examples/tree_sitter/tree_sitter.rs):
it parses a Rust file, resolves blocks and a symbol, demonstrates the cache
lifecycle (unchanged text reuses the tree, changed text re-parses), and
highlights a source snippet.

Next: the [block resolution](block.md), then [language detection](language.md)
and [highlighting](highlight.md).
