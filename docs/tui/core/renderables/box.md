# `BoxRenderable` — bordered container

A flexbox container with an optional border, background, and titles. This is
the workhorse *structure* widget: it holds children (which stack vertically
by default) and draws the frame around them. It participates in the
[`LayoutTree`](../layout.md) via its border + gap style.

```rust
BoxRenderable::new() -> Self
```

## Configuration

| Setter | Default | Effect |
|---|---|---|
| `set_border(enable: bool)` | `false` | Turn all four border sides on/off at once. |
| `set_border_sides(BorderSidesConfig)` | `NONE` | Per-side control (see [`border`](../lib/border.md)). |
| `set_border_style(BorderStyle)` | `Single` | `Single` \| `Double` \| `Rounded` \| `Heavy`. Resets any custom chars; enables all sides. |
| `set_custom_border_chars(BorderCharacters)` | `None` | Draw with a fully custom character set. |
| `set_border_color(Option<ColorInput>)` | `#FFFFFF` | Border color (alpha 0 = no border style). Also enables all sides. |
| `set_focused_border_color(Option<ColorInput>)` | `#00AAFF` | Border color when focused. |
| `set_background_color(Option<ColorInput>)` | `transparent` | Fill color inside the border (alpha 0 = no fill). |
| `set_should_fill(bool)` | `true` | Whether the background fill is drawn. |
| `set_title(Option<String>)` / `set_bottom_title(..)` | `None` | Text drawn on the top/bottom border. |
| `set_title_color(Option<ColorInput>)` | border color | Title color. |
| `set_title_alignment(TitleAlignment)` / `set_bottom_title_alignment(..)` | `Left` | `Left` \| `Center` \| `Right`. |
| `set_gap(Option<f32>)` / `set_row_gap` / `set_column_gap` | `None` | Flexbox gap between children (cells). |

Getters mirror each setter (`title()`, `border_color()`, `gap()`, …).

Note the quirk inherited from the original implementation: calling
`set_border_style` or `set_border_color` when no border was configured turns
**all** sides on (they check `if !self.border.top`). If you want a border
that is only on some sides, call `set_border_sides` **after** setting the
style/color.

## Children

```rust
panel.add_child(Box::new(child)) -> usize        // returns child index
panel.remove_child(id: &str)
// + insert_child_before(child, anchor_id) via the Renderable trait
```

Children stack in a column (the default flex direction). `to_taffy_style()`
exposes the widget's computed taffy style (border rect + gaps) for custom
layout.

## Rendering

`render_self` draws, in order: background fill (if any), then the border
(with the selected or custom characters), then the top/bottom titles. If
there is no border and no visible fill, it draws nothing — so a bare
`BoxRenderable` with no children is an invisible container.

## Example

```rust,ignore
use cosh_tui::core::renderables::box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::border::{BorderStyle, BorderSidesConfig};
use cosh_tui::core::lib::styled_text::string_to_styled_text;

let mut panel = BoxRenderable::new();
panel.set_border(true);
panel.set_border_style(BorderStyle::Rounded);
panel.set_border_color(Some("#00AAFF".into()));
panel.set_title(Some("Status".into()));
panel.set_title_alignment(cosh_tui::core::renderables::box::TitleAlignment::Center);
panel.set_border_sides(BorderSidesConfig::ALL);   // re-assert sides after style set

let mut label = TextRenderable::new(Some(string_to_styled_text("all good")));
panel.add_child(Box::new(label));

// panel.render_self(&mut buf, area);
```
