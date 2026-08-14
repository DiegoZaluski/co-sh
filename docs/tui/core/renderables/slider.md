# `SliderRenderable` — range slider

A track-and-thumb slider over a numeric range. Focusable. Use it for
settings like speed, volume, or zoom.

```rust
SliderRenderable::new(orientation: SliderOrientation) -> Self
pub enum SliderOrientation { Horizontal, Vertical }
```

## Range and value

| Method | Default | Effect |
|---|---|---|
| `set_value(f64)` | `0.0` | Set the thumb position (clamped to `min..=max`). |
| `value() -> f64` | | Read it back. |
| `set_min(f64)` / `set_max(f64)` | `0.0` / `100.0` | Change the range; clamps the current value into it. |
| `set_orientation(SliderOrientation)` | from `new` | Swap direction. |
| `set_view_port_size(f64)` | `10.0` | The visible window size — controls the thumb's *length* relative to the range. |

The thumb length is proportional to `view_port_size / (range + view_port_size)`
— a small viewport yields a large thumb (most of the content visible).

## Colors

```rust
set_track_color(Option<ColorInput>)   // default #252527
set_thumb_color(Option<ColorInput>)   // default #9a9ea3
track_color() -> RGBA
thumb_color() -> RGBA
```

## Rendering

`render_self` fills the track across the whole area, then draws the thumb
proportionally positioned within it. Rendering works at **half-cell
resolution** (a virtual track of `2 × render_size`) so the thumb can start
and end inside a cell; edge cells show half-block characters (`▌`, `▐`,
`▀`, `▄`) for smooth sub-cell positioning.

## Example

```rust,ignore
use cosh_tui::core::renderables::slider::{SliderRenderable, SliderOrientation};

let mut volume = SliderRenderable::new(SliderOrientation::Horizontal);
volume.set_min(0.0);
volume.set_max(100.0);
volume.set_value(42.0);
assert_eq!(volume.value(), 42.0);
volume.set_value(999.0);      // clamps to max
assert_eq!(volume.value(), 100.0);
```

Next: [scroll_bar — the scroll bar](scroll_bar.md).
