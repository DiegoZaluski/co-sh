# `term_screen::escape_parser` — parsing bytes into actions

The escape-sequence layer: turns raw terminal output bytes into typed
[`Action`]s with semantic meaning, and can re-encode those meanings back to
escape sequences. It is encoding/decoding **only** — it does not emulate a
terminal (that's the [`core`](core.md) state machine). (Ported from WezTerm —
see the [module overview](term_screen.md).)

## `Parser` — bytes → actions

Located at `term_screen::escape_parser::parser::Parser` (the module is
`parser`; it is not re-exported at the `escape_parser` root, where only
`CSI`, `Esc`, `OperatingSystemCommand`, `Error`, and `Result` are):

```rust,ignore
pub struct Parser { /* vtparse state */ }

pub fn new() -> Self

pub fn parse<F: FnMut(Action)>(&mut self, bytes: &[u8], callback: F)
    // feeds bytes, invoking the callback for each parsed Action.
    // State persists across calls, so split chunks are handled correctly.

pub fn parse_first(&mut self, bytes: &[u8]) -> Option<(Action, usize)>
    // first action + how many bytes it consumed

pub fn parse_as_vec(&mut self, bytes: &[u8]) -> Vec<Action>
pub fn parse_first_as_vec(&mut self, bytes: &[u8]) -> Option<(Vec<Action>, usize)>
```

## `Action` — the semantic unit

```rust,ignore
pub enum Action {
    Print(char),                          // one printable character
    PrintString(String),                  // run of printable characters
    Control(ControlCode),                 // C0/C1 control code (LF, CR, BS, ...)
    DeviceControl(DeviceControlMode),     // DCS sequences
    OperatingSystemCommand(Box<OperatingSystemCommand>),  // OSC: title, palette, ...
    CSI(CSI),                             // CSI sequences: cursor movement, SGR, ...
    Esc(Esc),                             // ESC sequences
    XtGetTcap(Vec<String>),               // terminal capability query
}
```

`Action::append_to(Vec<Action>)` coalesces adjacent `Print`s into a
`PrintString` to reduce allocation.

## The typed sequence types

```rust,ignore
pub struct CSI { /* private */ }
pub struct Esc { /* private */ }
pub enum EscCode { ... }
pub struct OperatingSystemCommand { /* private */ }
```

These are the *interpreted* forms: `CSI` parses the parameters after `ESC [`
into typed fields (privately — you read them via accessors or match the
public `ControlCode`/`OperatingSystemCommand` variants), `Esc` decodes
`ESC <char>` sequences, and `OperatingSystemCommand` covers OSC (window
title, color palette, hyperlinks, clipboard). `OneBased` and the
`CsiParam` helpers handle the parameter conventions (1-based indices,
defaults, optional parameters).

The submodules (`csi`, `esc`, `osc`, `color`, `hyperlink`, `error`) hold
these types and their serialization.

## Where the parser fits

`Terminal::advance_bytes` runs the `Parser` internally and feeds every
`Action` to its `Performer`, which mutates the `TerminalState`. If you want
the actions *without* applying them (e.g. to inspect or transform a
transcript), use `Parser` directly:

```rust,ignore
use cosh_sdk::term_screen::escape_parser::parser::Parser;

let mut parser = Parser::new();
let actions = parser.parse_as_vec(b"hello\x1b[31mred\x1b[0m");
// [Print('h'), Print('e'), ..., CSI(Sgr(Foreground(PaletteIndex(1)))),
//  Print('r'), ..., CSI(Sgr(Reset))]
```

Note the actions are individual `Print(char)` items — coalesce runs into a
`PrintString` with `Action::append_to` if you want fewer allocations.

---

Next: [input_types — keyboard and mouse encoding](input_types.md).
