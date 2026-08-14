# `ScrollBarRenderable` — scroll bar

A track-and-thumb scroll bar showing position within a scrollable document.
Focusable. Pair it with a `ScrollBoxRenderable` or any widget exposing
scroll state.

```rust
ScrollBarRenderable::new(orientation: ScrollBarOrientation) -> Self
pub enum ScrollBarOrientation { Vertical, Horizontal }
```

## Scroll model

| Method | Default | Effect |
|---|---|---|
| `set_scroll_size(f64)` | `0.0` | Total scrollable extent (content height). |
| `set_scroll_position(f64)` | `0.0` | Current offset; clamped to `scroll_size - viewport_size`. |
| `set_viewport_size(f64)` | `10.0` | Visible window (clamped ≥ 1). |
| `scroll_by(delta, relative)` | — | Move by `delta` cells, or by `delta × viewport_size` when `relative`. |

`set_scroll_size` and `set_viewport_size` re-clamp the current position, so
the thumb always stays in range.

## Colors

```rust
set_track_color(Option<ColorInput>)   // default #252527
set_thumb_color(Option<ColorInput>)   // default #9a9ea3
set_show_arrows(bool)                 // draw ▲/▼ (or ◄/►) at the ends
```

The arrow color is fixed at its default (`#9a9ea3`) — there is no public
setter for it in this version.

## Rendering

`render_self` fills the track across the area, then draws the thumb at the
position proportional to `scroll_position / (scroll_size - viewport_size)`.
The thumb size is proportional to `viewport_size / scroll_size` (a large
viewport = long thumb), clamped to at least 1 cell. Thumb characters:
`█` for the body, `▲`/`▼` (or `◄`/`►`) at the ends, `■` when the thumb is a
single cell.

## Example

```rust,ignore
use cosh_tui::core::renderables::scroll_bar::{ScrollBarRenderable, ScrollBarOrientation};

let mut bar = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
bar.set_scroll_size(100.0);
bar.set_viewport_size(20.0);
bar.set_scroll_position(30.0);    // clamps to max 80
assert_eq!(bar.scroll_position(), 30.0);
bar.scroll_by(1.0, true);         // +20 → 50
assert_eq!(bar.scroll_position(), 50.0);
```

Next: [scroll_box — the scrollable viewport](scroll_box.md).
