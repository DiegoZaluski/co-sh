# `ScrollBoxRenderable` — scrollable viewport container

A container that exposes scroll offsets and scrollable content dimensions
for its children. Use it to make a region of the UI scrollable: put the
content widget inside it, then drive `scroll_x`/`scroll_y` from input or a
scroll bar.

```rust
ScrollBoxRenderable::new() -> Self
```

## Scrolling

| Method | Effect |
|---|---|
| `set_scroll_x(i32)` / `set_scroll_y(i32)` | Set the offset (clamped ≥ 0). |
| `scroll_x()` / `scroll_y()` | Read the offsets. |
| `scroll_by(dx, dy)` | Adjust both offsets at once. |
| `set_content_size(width, height)` | Tell the box how big its content is. |

## Layout

`build_style()` returns a flex-column style with `overflow: scroll` on both
axes, so a `LayoutTree` reserves the box's area and scrolls its contents.

## Rendering

`render_self` is currently a **no-op**: the box itself draws nothing. It
exists to carry scroll state and to clip/offset its children when the
renderer drives layout; content widgets inside it handle their own drawing
(using the box's offsets via their own scroll setters, e.g.
[`TextRenderable::set_scroll_y`](text.md)).

## Example

```rust,ignore
use cosh_tui::core::renderables::scroll_box::ScrollBoxRenderable;

let mut viewport = ScrollBoxRenderable::new();
viewport.set_content_size(100, 500);   // content is 100×500
viewport.scroll_by(0, 40);             // view a window starting at y=40
assert_eq!(viewport.scroll_y(), 40);
```

Next: [code — syntax-highlighted code](code.md).
