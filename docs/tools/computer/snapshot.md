# `snapshot` — read the accessibility tree

`snapshot` is how the desktop becomes **legible**: it renders an application's
accessibility tree as an indented outline (or structured JSON), one line per
element, with role, name, value and state flags. It is read-only and safe in
every mode.

```
Computer::snapshot(&self, input: &ComputerSnapshot) -> Result<SnapshotOutput, String>
snapshot(metadata, ComputerSnapshot) -> Result<SnapshotOutput, String>
```

---

## The model: scope → traverse → render

1. **Scope.** `name` or `pid` — exactly one. This is the application whose
   tree you will read.
2. **Narrow (optional).** `selector` + `nth` cut the traversal to the matched
   subtree — a single dialog, a window's group — instead of the whole app.
3. **Traverse with a budget.** `max_depth` (default 12, hard cap 40) stops
   runaway trees (web-embedded UIs can be enormous); the output reports
   `elements`, the count it rendered, so you can tell "small app" from
   "truncated".
4. **Render.** `format` picks the shape:
   - `tree` (default) — a compact indented outline, one line per element;
   - `json` — structured nodes (`role`/`name`/`value`/`children`).

## Reading the outline

A line carries more than the name — **state flags** ride along in brackets:

```
window 'Export…'
  text_field 'Filename' [focused]
  button 'Export' [enabled=false]
  checkbox 'Open folder after' [checked]
```

Those flags are the vocabulary of [`wait`](wait.md): a button that renders
`[enabled=false]` is a `WaitState::Enabled` wait, not a blind retry. The
line itself hands you the selector — `button[name='Export']` — that
[`act`](act.md), [`control`](control.md) and [`screenshot`](screenshot.md)
accept directly.

## Edges

- **A snapshot is a moment.** UI changes; re-read before acting on stale
  lines. For "wait until it EXISTS", use [`wait`](wait.md), not snapshot
  loops.
- **Empty tree, real app?** Some apps ship accessibility bridges that are
  off by default (Chromium/Electron need `--force-renderer-accessibility`).
  The error tells you — see [`errors`](errors.md).

## See also

[`screenshot`](screenshot.md) for the same app as pixels ·
`cargo run --example computer-snapshot`
