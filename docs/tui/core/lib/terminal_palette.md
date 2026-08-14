# `terminal_palette` — OSC-4 palette detection

Queries the running terminal for its actual color palette over the **OSC-4
escape sequence** (`ESC ] 4 ; N ; color BEL`), so the UI can adapt its
colors to what the user's terminal really provides. This is a heavier,
opt-in capability — most widgets never touch it, and it requires a live
terminal with a response channel.

## Core types

```rust
pub type HexColor = Option<String>;

pub struct TerminalColors {
    pub palette: Vec<HexColor>,            // 16 entries: ANSI-16 (or per `size`)
    pub default_foreground: HexColor,
    pub default_background: HexColor,
    pub cursor_color: HexColor,
    pub mouse_foreground: HexColor,
    pub mouse_background: HexColor,
    pub tek_foreground: HexColor,
    pub tek_background: HexColor,
    pub highlight_background: HexColor,
    pub highlight_foreground: HexColor,
}

pub struct GetPaletteOptions { pub timeout_ms: u64, pub size: u8 }

pub trait TerminalPaletteDetector {
    fn get_palette(&mut self, options: GetPaletteOptions) -> Result<TerminalColors, String>;
}

pub struct NormalizedTerminalPalette {
    pub palette: Vec<RGBA>,
    pub default_foreground: RGBA,
    pub default_background: RGBA,
}
```

I/O plumbing is abstracted so the detector works under different terminal
wrappers:

```rust
pub type WriteFunction = Box<dyn Fn(&str) -> io::Result<()> + Send>;
pub type OscSubscriptionSource =
    Box<dyn Fn(Box<dyn Fn(&str) + Send>) -> Box<dyn FnOnce() + Send>>;

pub struct TerminalPaletteOptions {
    pub write_fn: Option<WriteFunction>,      // how to write the OSC query
    pub is_legacy_tmux: bool,                 // tmux < 3.0 workarounds
    pub is_tmux: bool,
    pub osc_source: Option<OscSubscriptionSource>,  // how to receive the reply
}
```

## The detector

```rust
pub struct TerminalPalette { /* … */ }

impl TerminalPalette {
    pub fn new(options: TerminalPaletteOptions) -> Self;
    pub fn detect_osc_support(&mut self, timeout_ms: u64) -> Result<bool, String>;
    pub fn detect(&mut self, options: Option<&GetPaletteOptions>) -> Result<TerminalColors, String>;
    pub const fn cleanup(&mut self) {}
}

pub fn create_terminal_palette(options: TerminalPaletteOptions) -> TerminalPalette
```

- `detect_osc_support` — probes whether the terminal answers OSC queries at
  all (with a `timeout_ms` limit; capped at 300 ms internally).
- `detect` — takes `Option<&GetPaletteOptions>` (`None` = defaults: 16
  colors, no timeout), queries the palette, and returns the parsed
  `TerminalColors`. If OSC is unsupported it returns an all-`None` palette
  rather than an error.

## Normalization helpers

```rust
pub fn normalize_terminal_palette(colors: Option<&TerminalColors>) -> NormalizedTerminalPalette
pub fn build_terminal_palette_signature(colors: Option<&TerminalColors>) -> String
```

`normalize_terminal_palette` converts the `Option<String>` hex entries into
concrete `RGBA`s (falling back to sane defaults for `None`).
`build_terminal_palette_signature` produces a stable string signature of the
palette — useful for cache keys or change detection.

## When to use it

Only when you need the *actual* palette of the terminal the user is running
— e.g. to pick contrast-safe highlight colors. It needs:

1. a way to write to the terminal (`write_fn`),
2. a way to receive the OSC reply (`osc_source`),
3. a live terminal that supports OSC-4.

Without those, detection fails gracefully with a `String` error. If you just
need *a* color, use [`rgba`](rgba.md) — this module is strictly optional
plumbing.

Back to [lib/ — the shared building blocks](primitives.md).
