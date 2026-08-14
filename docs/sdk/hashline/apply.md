# `apply` — executing edits on a text body

`apply_edits(text, &edits)` is the pure executor: given a text body and the
flat list of [`Edit`](types.md)s the parser produced, it returns the
post-edit text. No filesystem, no I/O — input in, output out.

```rust,ignore
pub fn apply_edits(text: &str, edits: &[Edit]) -> ApplyResult
```

## How it applies

Edits are bucketed by anchor line and applied **bottom-up** (highest line
first), so earlier anchors are never shifted out from under later ones:

1. **Boundary-balance repair** runs first (see below).
2. `insert head:` rows are collected and spliced at the top; `insert tail:`
   rows at the bottom (respecting a trailing-newline sentinel line).
3. Each anchor line gets its bucket applied: `before` inserts above,
   replacement rows in place of the line (when deleted), the original line
   kept when not deleted, `after` inserts below.

An empty edit list returns the input unchanged with `first_changed_line:
None` — the no-op signal the patcher uses to reject "Edits to X resulted in
no changes being made."

## Errors

- **Out-of-bounds anchor** — panics: `Line N does not exist (file has M
  lines)`. (The patcher surfaces this as a boxed error; the panic form is
  what `apply_edits` itself does, matching the "pure function" contract.)
- **Unresolved `Block` edit** — panics: `internal error: unresolved
  \`replace block\` edit reached the applier (resolve_block_edits was not
  run).` Block edits must be expanded by [`resolve_block_edits`](block.md)
  first; hitting this is a wiring bug.

## Boundary-balance repair

Models routinely miscount a replacement range's edges. The payload either
**restates a closing delimiter** that still lives just outside the range
(producing a duplicate `}` / `);` / `]`) or the range **deletes a closer
the payload never restates**. Both leave the file syntactically broken, and
both are the same defect: a replacement whose payload does not preserve the
deleted region's delimiter balance.

`apply_edits` therefore *repairs* the boundary before applying, but only
under strict conditions — a repair fires when:

- the payload's delimiter balance differs from the deleted region's, **and**
- one boundary operation (dropping an exact multi-line boundary echo or a
  single pure structural-closer line, or sparing a deleted pure
  structural-closer line) drives the difference to exactly zero while
  leaving the surrounding text byte-identical.

Each repair emits a warning so the caller knows content was adjusted:

> Auto-repaired a delimiter-balance mismatch in the replacement at line N:
> dropped 1 duplicated trailing payload line(s) already present below the
> range. Issue the payload as the final desired content only — never restate
> or omit a closing bracket bordering the range.

Balance-preserving edits are left strictly alone, and content lines are
never moved or lost — only duplicated closers dropped or deleted closers
spared.

## `PatchSection::apply_to` — the convenient path

For callers who have already validated the file content and just want the
result, [`PatchSection::apply_to`](input.md) ties the whole pure pipeline
together: parse (cached) → resolve blocks (with your
[`BlockResolver`](block.md)) → apply. `apply_partial_to` is the
streaming-tolerant counterpart: it parses with the tolerant parser and
drops unresolvable blocks instead of throwing, so a half-written file does
not blow up a live preview.
