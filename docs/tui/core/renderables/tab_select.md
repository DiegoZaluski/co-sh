# `TabSelectRenderable` — horizontal tab bar

A horizontal row of tabs with a selected tab, underline, scroll arrows, and
a description line for the selected tab. Focusable. Use it for view
switching.

```rust
TabSelectRenderable::new() -> Self
```

## Options

```rust
pub struct TabSelectOption {
    pub name: String,
    pub description: String,   // shown under the bar for the selected tab
}
```

| Method | Effect |
|---|---|
| `set_options(Vec<TabSelectOption>)` | Replace the tabs; clamps the selection. |
| `options() -> &[TabSelectOption]` | Read the tabs. |
| `set_selected_index(usize)` | Jump to an index (ignored if out of range). |
| `selected_index() -> usize` | Current index. |
| `selected_option() -> Option<&TabSelectOption>` | The selected tab. |
| `set_tab_width(u16)` / `tab_width()` | Width per tab, default `20`. |

## Navigation

```rust
move_left()   // previous tab, or wraps to the last when wrap_selection
move_right()  // next tab, or wraps to 0 when wrap_selection
```

> **Note:** like `SelectRenderable`, `wrap_selection` is an internal field
> with no public setter in this version (defaults to `false`), so navigation
> clamps at the first/last tab in practice.

## Appearance

| Setter | Default | Effect |
|---|---|---|
| `set_background_color` / `set_text_color` | transparent / white | Unselected tab style. |
| `set_focused_background_color` / `set_focused_text_color` | `#1a1a1a` / white | Style while focused. |
| `set_selected_background_color` / `set_selected_text_color` | `#334455` / `#FFFF00` | Selected tab style. |
| `set_show_description(bool)` | `true` | Draw the selected tab's description below the bar. |
| `set_show_underline(bool)` | `true` | Draw `▬` under the selected tab. |
| `set_show_scroll_arrows(bool)` | `true` | Draw `‹` / `›` when tabs overflow. |

## Rendering

Tabs are laid out left-to-right at `tab_width` columns each. The selected
tab gets the selected background/text and an underline row; the description
is drawn one row below the bar. When tabs overflow the width, scroll arrows
appear and the bar scrolls to keep the selection visible (tabs are
truncated with `…` when a single tab is too narrow).

## Example

```rust,ignore
use cosh_tui::core::renderables::tab_select::{TabSelectRenderable, TabSelectOption};

let mut tabs = TabSelectRenderable::new();
tabs.set_options(vec![
    TabSelectOption { name: "chat".into(), description: "Conversation view".into() },
    TabSelectOption { name: "files".into(), description: "Project explorer".into() },
    TabSelectOption { name: "search".into(), description: "Global search".into() },
]);
tabs.move_right();
assert_eq!(tabs.selected_index(), 1);
```

Next: [slider — range slider](slider.md).
