# `control` — pointer and keyboard pipelines

`control` is the tool that moves the *actual* pointer and types on the
*actual* keyboard. One step is EITHER a pointer action (click, double-click,
right-click, move, press/release, scroll, drag) OR a keyboard action
(`key`/`text`), and steps chain via `then` with shell `&&` semantics.

```
Computer::control(&self, input: &ComputerControl) -> Result<ControlOutput, String>
control(metadata, ComputerControl) -> Result<ControlOutput, String>
```

---

## The model: click, then type

Keyboard focus is the resource every keystroke spends — and the only
mechanism that reliably moves OS keyboard focus across toolkits is a real
synthetic click. That shapes the canonical pipeline:

```rust,ignore
use cosh_tools::computer::{Computer, types::*};

// `chain` below is the extension trait from examples/computer/control.rs —
// the crate ships the type, the example ships the fluent helper.
let pipeline = ComputerControl {
    app: Some("Files".into()),
    selector: Some("text_field[name='Filename']".into()),   // 1. click the field
    ..Default::default()
}
.chain(ComputerControl {                                     // 2. type into it
    text: Some("quarterly-report".into()),
    ..Default::default()
})
.chain(ComputerControl { wait: Some(600), ..Default::default() }) // 3. settle
.chain(ComputerControl { key: Some("enter".into()), ..Default::default() }); // 4. confirm
computer.control(&pipeline).await?;
// sent: click `text_field[name='Filename']` → typed "quarterly-report"
//       → waited 600 ms → pressed enter
```

Each step runs only if the previous succeeded; the first failure aborts the
chain and reports the point of failure. Targeting is **per step** — one step
may click in one app and the next type into another.

## The two target forms (per step)

- **Element form** — `app` (or `pid`) or `surface` + `selector`. The point
  resolves from the element's CURRENT bounds at dispatch time — never stale,
  and the click doubles as the focus move. This is the form to default to.
  `anchor` picks where inside the bounds the point lands: `center` (default),
  `top_left`, `top_right`, `bottom_left`, `bottom_right`.
- **Coordinate form** — bare `x`/`y` desktop pixels, usually mapped from a
  [`screenshot`](screenshot.md) via `desktop_origin`/`desktop_scale`. Reach
  for it only when the target has NO accessibility node (canvas, custom
  widget). It is **position-dependent** — the pointer may be elsewhere by
  the time the click lands — so [guardrails](#permissions) deny it in Build
  mode entirely: the approval dialog may move the pointer between measure
  and click.

## Pointer actions

`click` (with `count: 2` = double-click, independent of the action name),
`double_click`, `right_click`, `move`, `down`/`up` (press-and-hold pairs for
freehand choreography — `up` needs no target), `scroll` (needs `dx`/`dy`
wheel steps), and `drag` — a NATIVE drag: press at the start, interpolate
the pointer path at ~60 Hz across `duration_ms` (default 150, floor 50),
release at the end. A drag takes both ends in one of two forms — elements
(`selector` + `to_selector`) or coordinates (`x`/`y` + `x2`/`y2`) — never a
mix.

Modifiers ride on the step: `held: ["ctrl"]` turns a click into ctrl+click,
or a key into a chord — see [keyboard](keyboard.md) for the alias table.

## Inline waits

A standalone step `{"wait": 600}` pauses the chain — for the dialog a menu
item opens, for the ephemeral rename box to exist. Capped at 10 s per step;
for a CONDITION use [`wait`](wait.md), not sleeps.

## Permissions

The chain is checked AS A WHOLE before any step dispatches: element-form
chains are approved in Build mode (naming their root); a chain that starts
with a keyboard step is denied until an element click has set focus; a chain
containing a coordinate step stays Yolo/Command only.

## See also

[`act`](act.md) for semantic actions (often the better verb) ·
[mouse](mouse.md) for the pointer engine's validation details ·
[keyboard](keyboard.md) for typing rules · `cargo run --example computer-control`
