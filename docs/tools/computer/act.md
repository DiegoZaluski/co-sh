# `act` — semantic actions on accessibility elements

`act` invokes an element's **action layer** — the verbs the platform itself
exposes (press, toggle, expand, set_value, …) — instead of synthesizing
pointer motion. Where [`control`](control.md) reproduces what a hand would
do, `act` says what it MEANS: "press this button", "set this field", "open
this menu".

```
Computer::act(&self, input: &ComputerAct) -> Result<ActOutput, String>
act(metadata, ComputerAct) -> Result<ActOutput, String>
```

---

## The model: one step, then the next

A `ComputerAct` is ONE step of a pipeline. A semantic step carries:

- **a target** — `name`/`pid` (an application) or `surface` (a shell
  surface, see [`surface`](surface.md)) — plus a `selector` with optional
  `nth`;
- **an action** — [`ActAction`](types.md), defaulting to `press`. The full
  set: `press`, `focus`, `blur`, `toggle`, `select`, `expand`, `collapse`,
  `show_menu`, `increment`, `decrement`, `scroll_into_view`, `set_value`
  (+`value`), `set_numeric_value` (+`numeric_value`), `select_text`
  (+`range`, 0-based, `end` EXCLUSIVE), `type_text` (+`value`), and
  `perform_action` (+`value`, a platform-specific action name).
- **patience** — `timeout_ms` (default 3000) bounds the auto-wait for the
  app AND a visible+enabled match. Unlike [`snapshot`](snapshot.md)/
  [`screenshot`](screenshot.md), actions wait for actionability.

Steps chain via `then` with shell `&&` semantics: each next step runs only
if the previous succeeded, and the output joins the per-step reports with
` → ` (`set_value \`entry\` (app) → waited 400 ms → pressed \`button\``).

## Keyboard steps inside an act chain

A step with `key`/`text` (or a bare `wait`) is a KEYBOARD/WAIT step — no
selector, typing goes into whatever holds keyboard focus. That is the
validator split to internalize:

- **semantic steps** carry `selector` + action — validated by the semantic
  validator (payload checks run BEFORE dispatch, so a malformed call fails
  fast instead of auto-waiting for an element that was never the problem);
- **keyboard steps** carry `key`/`text`/`held` — validated by the shared
  [keyboard engine](keyboard.md) as the chain walks.

Feeding a keyboard step to the semantic validator is itself an error whose
guidance names the right engine. And because `act` has no pointer, a chain
that needs typing into a NEW field either uses `set_value` (which targets
an element directly) or does its focus move with a real
[`control`](control.md) click first.

## Choosing between `act` and `control`

| Reach for `act` when… | Reach for `control` when… |
|---|---|
| The element exposes the action (`snapshot` shows the role; buttons, fields, toggles usually do) | You need a real OS click to move keyboard focus |
| The intent is semantic ("check this", "set this") | The intent is positional (hover, drag, scroll a canvas) |
| You want auto-wait for actionability | You need pointer choreography |

## Edges

- `set_value` replaces the text entirely; `type_text` inserts at the caret.
- `select_text` ranges are 0-based with EXCLUSIVE end — `[0, 3]` selects
  three characters (the AT-SPI convention).
- `perform_action` is the escape hatch for platform-specific verbs
  (`"raise"`, macOS `AXCustomThing`-derived names).

## See also

[`control`](control.md) for pointer pipelines · [`surface`](surface.md) for
shell-level targets · `cargo run --example computer-act`
