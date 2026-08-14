# `TextTableRenderable` — bordered grid table

Draws a grid of styled cells with an optional box-drawing border, computed
column widths, and padding. Use it for tabular data.

```rust
TextTableRenderable::new(content: Option<TextTableContent>) -> Self
```

## Content model

```rust
pub type TextTableCellContent = Vec<TextChunk>;   // styled cell text
pub type TextTableContent = Vec<Vec<TextTableCellContent>>;  // rows × cols

let content: TextTableContent = vec![
    vec![vec![chunk("name")], vec![chunk("kind")]],
    vec![vec![chunk("lib.rs")], vec![chunk("rust")]],
];
```

Each cell is itself a list of [`TextChunk`](../lib/styled_text.md)s, so a
cell can carry multiple styled runs. Rows may have differing column counts;
the table uses the widest row's count and the widest cell per column.

## Configuration

| Method | Default | Effect |
|---|---|---|
| `set_content(TextTableContent)` | empty | Replace all cells. |
| `set_border(bool)` | `true` | Draw the grid border. |
| `set_border_style(BorderStyle)` | `Single` | `Single` \| `Double` \| `Rounded` \| `Heavy`. |
| `set_border_color(ColorInput)` | `#888888` | Border color. |
| `set_padding_x(u16)` | `1` | Horizontal padding inside each cell. |
| `set_default_fg(ColorInput)` / `set_default_bg(ColorInput)` | white / transparent | Cell default colors. |

(`padding_y` exists internally and defaults to 0.)

## Rendering behavior

- Column widths are the widest cell in each column (in characters), plus
  `2 × padding_x`.
- The border uses the style's character set with proper corners and tees;
  row separators use `cross`/`left_t`/`right_t`.
- Cells render their chunks left-to-right with each chunk's own `fg`
  (falling back to `default_fg`), clipped to the cell width.
- Rows taller than the area are clipped at the bottom edge.

## Example

```rust,ignore
use cosh_tui::core::renderables::text_table::TextTableRenderable;
use cosh_tui::core::lib::styled_text::{TextChunk, string_to_styled_text};

let chunk = |s: &str| vec![TextChunk { text: s.into(), fg: None, bg: None, attributes: 0, link: None }];

let mut table = TextTableRenderable::new(Some(vec![
    vec![chunk("file"), chunk("size")],
    vec![chunk("lib.rs"), chunk("1.2 kB")],
]));
table.set_border(true);
table.set_padding_x(2);
// table.render_self(&mut buf, area);
```

Next: [ascii_font — ASCII-art font](ascii_font.md).
