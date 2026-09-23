# `mouse` — the pointer engine behind `computer_control`

`mouse.rs` is the pointer engine: the validation and dispatch that
[`control`](control.md)'s pointer steps run through. You rarely construct
its calls by hand — you build a `ComputerControl` pointer step and the
engine takes over — but its rules are worth knowing, because they are the
reason a malformed step fails BEFORE any synthetic event is sent.

```
mouse::validate(step: &ComputerControl, tool: &str) -> Result<(), String>   // the gate
mouse::MIN_DRAG_MS = 50 · mouse::DEFAULT_DRAG_MS = 150                      // drag pacing
```

---

## The model: validate → resolve → dispatch

1. **Validate.** Every pointer step is checked against its action's
   requirements before touching the platform: a `drag` needs BOTH endpoints
   (`x2`/`y2` or `to_selector` — never a mix of forms), `scroll` needs
   `dx`/`dy`, an element-form step needs exactly one scope (`app`/`pid` or
   `surface`) with its `selector`, and `duration_ms` cannot undercut
   `MIN_DRAG_MS`.
2. **Resolve.** Element-form steps resolve the target's CURRENT bounds and
   land on the `anchor` point (half-open bounds: `top_left` is the last
   point inside, not the exclusive edge).
3. **Dispatch.** Events go through the process-wide input simulator —
   shared with the [keyboard engine](keyboard.md), so a down/up or drag
   never splits across devices.

## Action-by-action

| Action | Extra fields | Notes |
|---|---|---|
| `click` | `count`, `held` | `count: 2` = double-click; `held: ["ctrl"]` = ctrl+click. |
| `double_click` / `right_click` | `held` | Spelled conveniences over `click`+`count`. |
| `move` | — | Cursor repositioning; no click. |
| `down` / `up` | `button` | Press-and-hold pairs for freehand choreography; `up` needs no target. |
| `scroll` | `dx`, `dy` | Wheel steps; positive `dy` scrolls down. |
| `drag` | `x2`/`y2` or `to_selector`, `duration_ms` | NATIVE drag — interpolated at ~60 Hz, not a synthetic jump. |

## Edges

- **Validation errors carry the fix.** A half drag reads "`drag` requires an
  end point — `x2`/`y2` or `to_selector`" — the model can self-correct
  without a round trip.
- **Element form beats coordinates** whenever a node exists: bounds are
  re-resolved at dispatch, coordinates are not.
- The coordinate form is position-dependent and confined to
  Yolo/Command by [guardrails](control.md#permissions) — the reasoning
  lives on the [`control` page](control.md).

## See also

[`control`](control.md) for pipelines and permissions ·
`cargo run --example computer-mouse`
