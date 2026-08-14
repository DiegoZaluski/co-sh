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

> **Why bottom-up application:** If we applied edits from top to bottom, deleting or inserting lines would shift the line numbers of subsequent edits, causing them to target the wrong lines. By applying from bottom to top, we ensure that earlier edits (with lower line numbers) are never affected by the line shifts caused by later edits. This guarantees that each edit targets exactly the lines it was intended to modify.

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

---

## Summary

- `apply_edits(text, &edits)` is the pure executor: given text body and flat list of edits, returns post-edit text (no filesystem, no I/O).
- Application order: edits bucketed by anchor line and applied bottom-up (highest line first) so earlier anchors never shift out from under later ones.
- Boundary-balance repair: automatically fixes delimiter mismatches when payload's delimiter balance differs from deleted region's, under strict conditions (one boundary operation drives difference to zero).
- Repair fires when: payload delimiter balance ≠ deleted region balance AND one boundary operation (dropping duplicated closer or sparing deleted closer) drives difference to zero while keeping surrounding text byte-identical.
- Errors: out-of-bounds anchor (panic: "Line N does not exist"), unresolved `Block` edit (panic: "internal error: unresolved replace block edit reached the applier").
- `PatchSection::apply_to` is the convenient path tying pure pipeline together (parse → resolve blocks → apply); `apply_partial_to` is streaming-tolerant (tolerant parser, drops unresolvable blocks).
