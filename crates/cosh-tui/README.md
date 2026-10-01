# cosh-tui

[![Crates.io](https://img.shields.io/crates/v/cosh-tui.svg)](https://crates.io/crates/cosh-tui)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DiegoZaluski/co-sh/blob/main/LICENSE)

TUI crate for [cosh](https://github.com/DiegoZaluski/co-sh) — a coding agent for the terminal.

`cosh-tui` provides two layers for building rich terminal interfaces with [Ratatui](https://crates.io/crates/ratatui): `core` — a widget/renderable library (styled text, markdown, diffs, inputs, layout, colors) that renders into a `ratatui::buffer::Buffer` — and `solid` — a SolidJS-inspired reactive layer with a component catalogue, slots and a reconciler. The crate performs no terminal I/O: your app owns the event loop, and every widget implements a single `Renderable` trait that paints into a buffer.

## Highlights

- **Markdown rendering** — full `MarkdownRenderable` built on `pulldown-cmark` with a themable palette (accents, syntax colors, tables, links) and plain-text extraction.
- **Diff rendering** — `DiffRenderable` with unified and split view modes and typed line classification.
- **Syntax highlighting** — tree-sitter based (via [`cosh-sdk`](https://crates.io/crates/cosh-sdk)) with a named `SyntaxStyle` registry.
- **Color system** — `RGBA`/`ColorInput` parsing plus terminal palette detection (OSC support probing and normalization).
- **Text engineering done right** — grapheme clustering, display-width measurement, word wrap, link detection, and packed text attributes.
- **Flexbox layout** — `LayoutTree` backed by `taffy`.
- **A rich widget set** — markdown, diff, code, text, textarea, input, box, scrollbox, select, slider, tab select, ASCII font.
- **Reactive layer (`solid`)** — SolidJS-style component registration, slots and a reconciler.

## Usage

`Renderable` widgets paint into any `ratatui::buffer::Buffer` — no terminal needed:

```rust
use cosh_tui::core::renderable::Renderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

let widget: &dyn Renderable = &my_widget;
let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
widget.render_self(&mut buf, Rect::new(0, 0, 60, 10));
```

Runnable examples render widgets to a buffer and print them to stdout — run with `cargo run --example core` (colors, borders, styled text, layout, full widget set) or `cargo run --example solid` (reactive layer).

## Requirements

- Rust 2024 edition (≥ 1.85)
- Ratatui 0.30 host app that owns the terminal backend and event loop

Note: this crate depends on its sibling `cosh-sdk` via a path dependency, so it is not yet installable from crates.io — it is published-facing, but until the workspace dependency is resolved, use it from the [repository](https://github.com/DiegoZaluski/co-sh) directly.

## Related crates

- [`cosh-sdk`](https://crates.io/crates/cosh-sdk) — agent harness SDK (syntax highlighting, shared engines)
- [`cosh-tools`](https://crates.io/crates/cosh-tools) — tool implementations
- [`cosh-recall`](https://crates.io/crates/cosh-recall) — memory and context recall

## License

Apache-2.0. See [LICENSE](https://github.com/DiegoZaluski/co-sh/blob/main/LICENSE).
