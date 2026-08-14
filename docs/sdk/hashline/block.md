# `block` — resolving `replace block N:` edits

`replace block N:` is a convenience that lets an edit target a **syntactic
block** instead of an explicit line range. The parser can't expand it —
the line span of "the block beginning on line N" is unknown until the file
text and language are available. Expansion happens here, at every
apply/preview boundary that has text:

```rust,ignore
pub fn resolve_block_edits(
    edits: &[Edit],
    text: &str,
    path: &str,                 // language inferred from the extension
    resolver: Option<BlockResolver>,
    options: Option<ResolveBlockEditsOptions>,
) -> Vec<Edit>
```

## How it works

1. If no edit is a `Block` variant, the input is returned unchanged (fast
   path).
2. For each `Block` edit, build a `BlockResolverRequest { path, text,
   line }` and call the injected `BlockResolver`.
3. On success (`BlockSpan { start, end }`), emit the **exact same**
   expansion `replace start..end:` would produce: one `before_anchor`
   replacement insert per payload row at `start`, then one delete per line
   across `[start, end]`. An empty `payloads` (from `delete block N`) emits
   no inserts — a pure range deletion.
4. After it runs, no `Block` edits remain, so the applier and recovery only
   ever see resolved edits.

## The resolver seam

`BlockResolver` is a plain function pointer (`fn(BlockResolverRequest) ->
Option<BlockSpan>`). The hashline core declares the contract; the **host
injects the implementation** — typically tree-sitter backed. `None` means
"no block can be resolved": unrecognized language, blank/out-of-range line,
no node begins on line N (e.g. a lone closing delimiter), or the resolved
subtree has a syntax error.

> **Host wiring note:** nothing in cosh-sdk wires a resolver by default —
> `Patcher::new(fs, store, None)` runs without block support, and a
> `replace block N:` then fails with `\`replace block N:\` is not available
> here (no tree-sitter block resolver is configured). Use \`replace N..M:\`
> with an explicit range.` The real resolver lives in the host application
> (cosh's own `tree_sitter` module) and is passed in as the third argument.

## Throw vs. Drop

`ResolveBlockEditsOptions { on_unresolved }` controls what happens when a
block can't be resolved:

| Action | Behavior | Used by |
|---|---|---|
| `Throw` (default) | Panics with a resolution diagnostic | Authoritative apply + final preview — an unresolvable block is a real error |
| `Drop` | Silently skips the edit | Streaming previews — a half-written file must not throw |

The throw message names the offending line and steers back to an explicit
range:

> \`replace block N:\` could not resolve a syntactic block beginning on line
> N. The language may be unsupported, the line may be blank or a closing
> delimiter, or the block may not parse. Use \`replace N..M:\` with the
> block's explicit end line instead.

## Block resolution against the right text

The patcher resolves block anchors against the text the section tag names
— when the live file matches the tag, that's the live content; when the
file drifted, the ranges are resolved against the tagged snapshot's text so
they flow through the 3-way-merge recovery correctly. If the snapshot is
unavailable, the edit is rejected as a mismatch rather than placed blindly.
