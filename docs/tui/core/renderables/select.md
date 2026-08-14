# `SelectRenderable` — vertical option list

A scrollable, vertical list of options with an optional description line per
option. Focusable. Use it for pickers and menus.

```rust
SelectRenderable::new() -> Self
```

## Options

```rust
pub struct SelectOption {
    pub name: String,          // the selectable label
    pub description: String,   // secondary line shown under the name
}
```

| Method | Effect |
|---|---|
| `set_options(Vec<SelectOption>)` | Replace the list; clamps the selection. |
| `options() -> &[SelectOption]` | Read the list. |
| `set_selected_index(usize)` | Jump to an index (ignored if out of range). |
| `selected_index() -> usize` | Current index. |
| `selected_option() -> Option<&SelectOption>` | The selected option. |

## Navigation

| Method | Behavior |
|---|---|
| `move_up(steps)` | Move up, clamping at 0 (or wrapping to the last). |
| `move_down(steps)` | Move down, clamping at the end (or wrapping to 0). |

> **Note:** wrapping is currently *internal* — the `wrap_selection` field
> defaults to `false` and has no public setter in this version, so selection
> clamps at the bounds in practice.

## Appearance

| Setter | Default | Effect |
|---|---|---|
| `set_background_color` | transparent | Row background. |
| `set_text_color` | `#FFFFFF` | Row text. |
| `set_focused_background_color` / `set_focused_text_color` | `#1a1a1a` / white | Style when the widget is focused. |
| `set_selected_background_color` / `set_selected_text_color` | `#334455` / `#FFFF00` | Style of the selected row. |
| `set_show_description(bool)` | `true` | Show the description line under each name. |
| `set_show_selection_indicator(bool)` | `true` | Draw `▶ ` before the selected row. |
| `set_show_scroll_indicator(bool)` | `false` | Reserved for a scroll indicator. |

The selected row uses the `selected_*` colors; non-selected rows use the
base/focused colors. Selection auto-scrolls so the selected row stays
visible (it tries to keep it near the vertical center of the viewport).

## Rendering

`render_self` draws each visible option as one row (name, prefixed by the
indicator) plus a second dimmed row for its description when enabled and
space allows. The viewport is the `area`; only options that fit are drawn.

## Example

```rust,ignore
use cosh_tui::core::renderables::select::{SelectRenderable, SelectOption};

let mut select = SelectRenderable::new();
select.set_options(vec![
    SelectOption { name: "rust".into(), description: "A systems language".into() },
    SelectOption { name: "go".into(), description: "Compiled and concurrent".into() },
    SelectOption { name: "python".into(), description: "Interpreted scripting".into() },
]);
select.move_down(1);
assert_eq!(select.selected_option().map(|o| o.name.as_str()), Some("go"));
```

Next: [tab_select — horizontal tab bar](tab_select.md).
