# The `computer` module: seeing and controlling the desktop

This module is the desktop-control toolbox of `cosh-tools`. It gives an agent
eyes (what is on screen, as a tree and as an image), a voice (semantic actions
and synthetic input), and patience (blocking waits) over the machine it runs
on:

| Operation | What it does |
|---|---|
| [`apps`](apps.md) | List running applications, or resolve which element holds keyboard focus. |
| [`snapshot`](snapshot.md) | Read an application's accessibility tree as an outline or JSON. |
| [`screenshot`](screenshot.md) | Capture the desktop — optionally drawing boxes on elements and returning copy-ready selectors. |
| [`wait`](wait.md) | Block until an element reaches a state; time out with a diagnosis. |
| [`act`](act.md) | Invoke an element's semantic action layer (press, toggle, set_value, …). |
| [`control`](control.md) | Chain pointer and keyboard steps — real clicks, real typing. |
| [`surface`](surface.md) | Target the desktop itself: menu bar, taskbar, tray, flyouts. |
| [`errors`](errors.md) | How failures are rendered: every error teaches the next step. |

---

## The mental model: see → address → act → wait

Almost every desktop task follows the same loop, and the module is shaped
around it:

1. **See** what is there — [`computer_snapshot`](snapshot.md) for the
   accessibility tree (the structured truth), [`computer_screenshot`](screenshot.md)
   for pixels (what the user actually perceives).
2. **Address** the element you care about. Every tool accepts the same
   CSS-like selector grammar — `button[name='Export']`,
   `window[name='Main'] > group`, `list_item:nth(3)` — so a snapshot line
   converts directly into an act or control call.
3. **Act** on it — [`computer_act`](act.md) when the element exposes a
   semantic action (most buttons, fields and toggles do);
   [`computer_control`](control.md) when you need a real OS click (the one
   reliable way to move keyboard focus) or raw pointer work.
4. **Wait** for the consequence — [`computer_wait`](wait.md) blocks on the
   element state you expect (a dialog appearing, a spinner detaching) instead
   of re-polling in a loop.

```rust,ignore
use cosh_tools::computer::{Computer, types::*};

let computer = Computer::new();

// 1. See it.
let shot = computer.screenshot(&ComputerScreenshot {
    app: Some("Reports".into()),
    annotate: true,
    ..Default::default()
}).await?;

// 2. Address it — the screenshot legend hands you selectors:
//    "B7 → button[name='Export']"

// 3. Act on it (auto-waits for the element to be visible + enabled).
computer.act(&ComputerAct {
    name: Some("Reports".into()),
    selector: Some("button[name='Export']".into()),
    ..Default::default()
}).await?;

// 4. Wait for the consequence.
computer.wait(&ComputerWait {
    name: Some("Reports".into()),
    selector: Some("progress_bar[name='Exporting…']".into()),
    state: Some(WaitState::Detached),
    ..Default::default()
}).await?;
```

---

## Targeting: three roots, one grammar

Every addressing-capable tool resolves its selector from one of three roots,
selected by the scope fields it was given:

| Root | Fields | Means |
|---|---|---|
| Application | `name` or `pid` | The accessibility tree of one running app. |
| Shell surface | `surface` | Part of the desktop itself — [`surface`](surface.md). |
| Whole tree | *(none)* | Unscoped (screenshot only): the full desktop. |

The **element form** (`scope` + `selector`) is tree-grounded: the point of
interaction resolves from the element's *current* bounds at dispatch time and
can never go stale. The **coordinate form** (`x`/`y` desktop pixels) exists
for node-less targets — canvas widgets, a point measured off a screenshot —
and is position-dependent, so [guardrails](#permissions) confine it.

> **Why clicks matter.** Keyboard focus is the resource every keystroke
> spends. The only mechanism that reliably moves OS keyboard focus across
> toolkits is a real synthetic click — which is why the canonical
> [`computer_control`](control.md) pipeline is *click the field, then type*.

---

## Permissions

Read-only tools (`apps`, `snapshot`) are safe in every mode — they move
nothing and hand back no coordinates. `screenshot` is the subtle one: its
plain captures (full display / `region`) hand the model **pixel
coordinates**, so Build mode denies them — while `annotate: true` captures
are allowed (selector-grounded: the model acts via the legend's selectors,
never via pixels). Input tools are gated by mode:

- **Build mode**: element-form chains are approved per call, naming their
  root; coordinate-form steps are denied outright (the approval dialog may
  move the pointer between measure and click, so coordinates go stale).
- **Yolo / Command**: everything dispatches without a dialog.

Failures are never bare errors — [`errors`](errors.md) renders each failure
with the next-step guidance and, for waits, the last observed state.

---

## Layout

The source tree is the index:

| Source | Docs | Runnable example |
|---|---|---|
| `apps.rs` | [apps.md](apps.md) | `cargo run --example computer-apps` |
| `snapshot.rs` | [snapshot.md](snapshot.md) | `cargo run --example computer-snapshot` |
| `screenshot.rs` | [screenshot.md](screenshot.md) | `cargo run --example computer-screenshot` |
| `wait.rs` | [wait.md](wait.md) | `cargo run --example computer-wait` |
| `act.rs` (the touch engine) | [act.md](act.md) | `cargo run --example computer-act` |
| `control.rs` | [control.md](control.md) | `cargo run --example computer-control` |
| `mouse.rs` | [mouse.md](mouse.md) | `cargo run --example computer-mouse` |
| `keyboard.rs` | [keyboard.md](keyboard.md) | `cargo run --example computer-keyboard` |
| `surface.rs` | [surface.md](surface.md) | `cargo run --example computer-surface` |
| `errors.rs` | [errors.md](errors.md) | `cargo run --example computer-errors` |

Wire types live in `types.rs`; the [type reference](types.md) documents the
input and output structures per tool.
