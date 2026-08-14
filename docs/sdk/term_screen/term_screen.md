# The `term_screen` module: virtual terminal model

`term_screen` is a **full virtual terminal emulator model** — it parses the
escape sequences a program writes to a terminal, maintains the resulting
screen state (cells, attributes, scrollback, cursor), and can encode keyboard
and mouse input back into the wire format a terminal program expects.

```text
program output bytes ──▶ Terminal::advance_bytes ──▶ Screen (cells, lines, scrollback)
                                                          │
keyboard / mouse ──▶ TerminalState (input encoding) ──▶   │  written to the PTY
```

It is the engine that powers the `vision_terminal` tool in `cosh-tools`
(rendering raw terminal output to structured cells), and it can power
anything else that needs to *understand* what a terminal screen would show.

## Ported from WezTerm

> **This module is a port of WezTerm's terminal model.**
> Every line of its implementation originates from the
> [wezterm](https://github.com/wezterm/wezterm/) project (home:
> `wezterm/wezterm`, crate `wezterm-terminal`), developed by Wez Furlong and
> contributors, and was vendored here with its original module structure
> intact. We did not write this code; we ship it as an internal dependency.
> This documentation therefore covers **only the public API surface** — how
> to use the model — and deliberately avoids re-explaining the internals.
> For implementation details, consult the WezTerm source and docs.

## Module layout

| Module | Contents | Documented on |
|---|---|---|
| [`core`](core.md) | The entry point: `Terminal`, `TerminalState`, `Screen`, `TerminalSize`, `TerminalConfiguration`, row-index types, handlers | [core](core.md) |
| [`surface`](surface.md) | The display model: `Line`, `Cell`, `CellAttributes` | [surface](surface.md) |
| [`escape_parser`](escape_parser.md) | Escape-sequence parsing: `Parser`, `Action`, `CSI`/`Esc`/`OperatingSystemCommand` | [escape_parser](escape_parser.md) |
| [`input_types`](input_types.md) | Keyboard/mouse input encoding: `KeyCode`, `KeyboardEncoding`, `KeyCodeEncodeModes` | [input_types](input_types.md) |
| `cell`, `bidi`, `char_props`, `color_types` | Supporting types re-exported through the above (cell attributes, colors, bidi, character properties) | [surface](surface.md) / [core](core.md) |

## The shape of the API

Three layers, used in this order:

1. **Feed** — `Terminal::new(size, config, term_program, term_version, writer)`,
   then `advance_bytes(...)` for every chunk of program output (chunks need
   not be complete escape sequences).
2. **Read** — `Terminal::screen()` returns the [`Screen`](core.md): visible
   and scrollback lines, per-cell text and attributes, the cursor, and the
   semantic zones.
3. **Interact** — via the `TerminalConfiguration`, `Clipboard`,
   `AlertHandler`, and `DeviceControlHandler` traits, plus the input encoding
   types for writing keyboard/mouse events to the connected program.

The `Terminal` derefs to `TerminalState`, so everything on the state is
available directly on the terminal.

## A note on scope

`term_screen` provides the **model only** — no GUI, no PTY management. You
supply a `std::io::Write` for input and feed it output bytes; rendering is
yours.

---

## Example

A complete runnable walkthrough lives at
[`examples/term_screen/term_screen.rs`](../../../crates/cosh-sdk/examples/term_screen/term_screen.rs):
it builds a terminal, feeds it a realistic program transcript with escape
sequences, reads back the screen, and demonstrates the parser API directly.

Next: [core — Terminal, Screen, configuration](core.md).
