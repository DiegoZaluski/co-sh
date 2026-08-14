# `renderables` — the widget library

Ready-made [`Renderable`](../renderable.md) implementations. Each widget owns
its state, draws itself into a [`Buffer`](https://docs.rs/ratatui) within a
given `Rect`, and — for interactive widgets — exposes methods to mutate its
state (typing, moving a selection, scrolling).

| Widget | Source file | Page | Focusable |
|---|---|---|---|
| [`ASCIIFontRenderable`](ascii_font.md) | `ascii_font.rs` | ASCII-art font | no |
| [`BoxRenderable`](box.md) | `box.rs` | Bordered container with children | no |
| [`CodeRenderable`](code.md) | `code.rs` | Syntax-highlighted code | no |
| [`DiffRenderable`](diff.md) | `diff.rs` | Unified/split diffs | no |
| [`InputRenderable`](input.md) | `input.rs` | Single-line input | **yes** |
| [`MarkdownRenderable`](markdown/markdown.md) | `markdown/` | Full markdown renderer | no |
| [`ScrollBarRenderable`](scroll_bar.md) | `scroll_bar.rs` | Scroll bar | yes |
| [`ScrollBoxRenderable`](scroll_box.md) | `scroll_box.rs` | Scrollable viewport container | no |
| [`SelectRenderable`](select.md) | `select.rs` | Vertical option list | **yes** |
| [`SliderRenderable`](slider.md) | `slider.rs` | Range slider | yes |
| [`TabSelectRenderable`](tab_select.md) | `tab_select.rs` | Horizontal tab bar | **yes** |
| [`TextRenderable`](text.md) | `text.rs` | Styled multi-line text | no |
| [`TextNodeRenderable`](text_node.md) | `text_node.rs` | Tree of styled text nodes | no |
| [`TextTableRenderable`](text_table.md) | `text_table.rs` | Bordered grid table | no |
| [`TextareaRenderable`](textarea.md) | `textarea.rs` | Multi-line editor | **yes** |

## The shared shape

Every widget follows the same pattern (documented in
[`renderable`](../renderable.md)):

1. **Construct** — `Widget::new(..)` mints a unique `num`, an `id` like
   `"box-1"`, and default styling.
2. **Configure** — builder-style `set_*` methods for content, colors,
   behavior. Colors are `Option<ColorInput>` (see [`rgba`](../lib/rgba.md)).
3. **Compose** — container widgets (`BoxRenderable`, `ScrollBoxRenderable`)
   accept children via `add_child(Box<dyn Renderable>) -> usize`.
4. **Draw** — `render_self(&self, buf, area)` writes into the buffer.

Interactive widgets (input, select, tab select, textarea, scroll bar,
slider) additionally expose **state mutation methods** — `insert_text`,
`move_down`, `move_left` — that an application drives from its input loop.

## Composing a screen

```rust,ignore
use cosh_tui::core::renderables::box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::styled_text::string_to_styled_text;

let mut panel = BoxRenderable::new();
panel.set_border(true);
panel.set_title(Some("Output".into()));

let mut text = TextRenderable::new(Some(string_to_styled_text("hello")));
panel.add_child(Box::new(text));

// render into a ratatui buffer:
// panel.render_self(&mut buf, area);
```

Start with [text](text.md) (plain content) and [box](box.md) (structure);
add [markdown](markdown/markdown.md) for formatted content, and
[input](input.md)/[select](select.md)/[textarea](textarea.md) for
interaction.
