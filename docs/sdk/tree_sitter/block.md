# Block and symbol resolution

The two most-used operations in the module. Both produce a
[`BlockSpan`](../hashline/types.md) — a 1-indexed inclusive line range —
from a parsed tree.

## `resolve_block(path, text, line)` — the block on line N

```rust,ignore
pub fn resolve_block(&self, path: &str, text: &str, line: u32) -> Option<BlockSpan>
```

Answers "what syntactic block begins on `line`?" This is the implementation
behind hashline's `replace block N:` (the host passes it to the patcher as a
[`BlockResolver`](../hashline/block.md)).

The algorithm:

1. Convert the 1-indexed `line` to a **byte offset** (Tree-sitter positions
   are bytes, not lines).
2. Find the deepest named node that **starts** at that byte.
3. **Climb to the largest structure that still starts on the target line** —
   walk up the tree while the parent also starts on the same row, stopping
   before entering a previous line or hitting the file root.
4. Confirm the resolved node still starts on the target line; otherwise
   return `None`.

The resolution only succeeds for a line where a syntactic node actually
*begins*. For `replace block N:` that is exactly the contract — `N` must be
an opening line (a `fn`/`struct`/`class` header, a statement start). Interior
lines (a body statement, a closing brace) and **blank lines** return `None`.

Example — `resolve_block("f.rs", src, 1)` on a file whose first line is
`fn main() {` returns a span covering the whole `main` function; line 2 (a
body statement) returns `None`.

## `resolve_symbol(path, text, name)` — the definition named X

```rust,ignore
pub fn resolve_symbol(&self, path: &str, text: &str, name: &str) -> Option<BlockSpan>
```

Answers "where is the definition of `name`?" — used by symbol-targeted
file reads (cosh-tools `fs_read` with a `symbol` target). It walks the tree
in **pre-order** looking for the first node whose `name` field matches, so
the *outermost* matching definition wins (a module before a nested symbol).
Returns `None` when nothing matches.

## `with_tree(path, text, f)` — raw tree access

```rust,ignore
pub fn with_tree<R>(&self, path: &str, text: &str, f: impl FnOnce(&Tree, &str) -> R) -> Option<R>
```

Runs `f` with the parsed `Tree` and its source text, sharing the same cache
lifecycle (miss → full parse, hit unchanged → reuse, hit changed →
incremental re-parse). This is the escape hatch for consumers that need to
walk and match nodes themselves (the AST rewrite engine in cosh-tools uses
it), and the basis for the higher-level helpers.

## The cache contract

All three methods go through one path:

```
lock the cache
  pop entry for path
    ├─ none         → detect language, full parse, new entry
    ├─ same text    → reuse the tree
    └─ changed text → incremental parse with the old tree (falls back to None,
                      restoring the old entry, if the language is unsupported
                      or parsing fails)
  run your closure with the entry
put the entry back
```

Two consequences worth knowing:

- **The cache is invalidated by writers.** `rollback::restore` and the
  cosh-tools write/edit paths call `tree_sitter().invalidate(path)` after
  changing a file, so the next read never sees a stale tree. If you mutate a
  file yourself, call `invalidate` too.
- **Incremental re-parses reuse the old tree**, which is much cheaper than a
  full parse for a large file with a small edit — this is the whole point
  of keeping the tree around between calls.
