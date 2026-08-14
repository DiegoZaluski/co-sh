# `scrollback` — scrollback snapshot writer

Types for capturing a solid component tree into a **scrollback snapshot** —
the rendered output preserved above the visible terminal area. **Port
status: scaffolded.** The types and signatures are in place, but the
writer produces an empty snapshot and the main entry point is a no-op.

## Types

```rust
pub struct SolidScrollbackWriterOptions {
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub row_columns: Option<u16>,
    pub start_on_new_line: Option<bool>,
    pub trailing_newline: Option<bool>,
}
// Default: start_on_new_line = Some(true), everything else None.

pub struct ScrollbackSnapshot {
    pub root: RootRenderable,
    pub width: u16,
    pub height: u16,
    pub row_columns: Option<u16>,
    pub start_on_new_line: bool,
    pub trailing_newline: bool,
}

pub struct ScrollbackRenderContext {
    pub width: u16,
    pub tail_column: u16,     // where the current line tail starts
}

pub type ScrollbackWriter = Box<dyn Fn(ScrollbackRenderContext) -> ScrollbackSnapshot>;
```

## Functions

```rust
pub fn create_scrollback_writer(
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
) -> ScrollbackWriter

pub fn write_solid_to_scrollback(
    _renderer: (),
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
)
```

- `create_scrollback_writer` currently returns a writer that always produces
  an **empty snapshot**: `root: RootRenderable::new()`, `height: 1`,
  `trailing_newline: false`, sized from the context's `width`.
- `write_solid_to_scrollback` is a no-op stub.

The design intent (per the source comments): integrate with the ratatui
backend and the `LayoutTree` once the renderer exposes a scrollback surface
API equivalent to the original `OptimizedBuffer` path — i.e. render the
component tree into an off-screen buffer and hand it to the terminal's
scrollback.

## Practical guidance

Scrollback capture is not yet functional; treat these as forward-compatible
API scaffolding. To render content off-screen today, render widgets into an
in-memory `ratatui::buffer::Buffer` yourself (the same approach the
[`core` example](../../../crates/cosh-tui/examples/core/core.rs) uses).

Next: [time_to_first_draw — TTFD marker](time_to_first_draw.md).
