# `term_screen::input_types` — keyboard and mouse input

The types for **encoding user input** into the wire format a terminal
program understands. The [`TerminalState`](core.md) uses these to write
keyboard and mouse events to the `writer` you passed to `Terminal::new`.
(Ported from WezTerm — see the [module overview](term_screen.md). Note: this port is
a **minimal stub** of the upstream `termwiz::input` types — it keeps the
shapes the terminal state needs, not the full upstream surface.)

## `KeyCode` — a key press

```rust,ignore
pub enum KeyCode {
    Char(char),
    Enter, Tab, Backspace, Escape,
    UpArrow, DownArrow, LeftArrow, RightArrow,
    Home, End, PageUp, PageDown, Insert, Delete,
    F(u8),
    ApplicationUpArrow, ApplicationDownArrow,   // application-cursor variants
    ApplicationLeftArrow, ApplicationRightArrow,
    NumpadUpArrow, NumpadDownArrow, NumpadLeftArrow, NumpadRightArrow,
    KeypadBegin, KeypadEnter, KeypadHome, KeypadEnd,
    KeypadPageUp, KeypadPageDown, KeypadInsert, KeypadDelete, KeypadCenter,
    KeypadPlus, KeypadMinus, KeypadMultiply, /* ... and more keypad keys */
}
```

A flat enum of concrete keys — printable characters via `Char`, everything
else as named variants.

## `KeyboardEncoding` — the wire protocol

```rust,ignore
pub enum KeyboardEncoding {
    Xterm,                          // default
    CsiU,                           // CSI-u (fixterms)
    Kitty(KittyKeyboardFlags),      // kitty keyboard protocol
    Win32,                          // legacy Windows console style
}
```

The active encoding is negotiated with the program over the wire; the
current state is available via `TerminalState::get_keyboard_encoding()`.

## `KeyCodeEncodeModes` — encoding options

```rust,ignore
pub struct KeyCodeEncodeModes {
    pub key: KeyCode,
    pub mods: Modifiers,                 // shift / ctrl / alt / super
    pub encoding: KeyboardEncoding,
    pub newline_mode: bool,
    pub application_cursor_keys: bool,
    pub modify_other_keys: bool,
}
```

The full context needed to encode one key press: which key, which modifiers,
which protocol, and the mode flags that change the encoding (newline mode,
application-cursor mode, and the `modifyOtherKeys` extension).

## Mouse

Mouse input encoding lives on the terminal state (the `terminalstate`
module's `mouse` submodule) — scroll-wheel events, button presses, and motion
are encoded according to the mode the program requested and written to the
same `writer`.

---

Back to the [module overview](term_screen.md).
