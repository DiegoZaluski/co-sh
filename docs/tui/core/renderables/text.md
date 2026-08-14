# `TextRenderable` — styled multi-line text

Draws a [`StyledText`](../lib/styled_text.md) — a list of styled chunks —
into an area, with word wrapping, scrolling, and truncation. The basic "put
text on screen" widget.

```rust
TextRenderable::new(content: Option<StyledText>) -> Self
```

`None` starts with empty text (a single empty default chunk). For plain
text, wrap it first: `string_to_styled_text("…")`.

## Content

| Method | Effect |
|---|---|
| `set_content(StyledText)` | Replace all chunks (marks the text as manually styled). |
| `content() -> &StyledText` | Read the current text. |
| `chunks() -> &[TextChunk]` | The raw chunk list. |
| `plain_text() -> String` | Concatenated chunk text — the unstyled view. |
| `clear()` | Reset to empty text and drop children. |

Chunk colors resolve as `chunk.fg.unwrap_or(default_fg)` — `None` inherits
the widget's default, `Some(rgba)` overrides (see
[`styled_text`](../lib/styled_text.md)).

## Defaults

| Setter | Default | Effect |
|---|---|---|
| `set_fg(RGBA)` | white | Default foreground for chunks without one. |
| `set_bg(RGBA)` | transparent | Default background. |
| `set_attributes(u32)` | `0` | Attributes OR'd into every chunk (`chunk.attributes \| default`). |
| `set_wrap_mode(WrapMode)` | `Word` | `None` \| `Char` \| `Word` wrapping. |
| `set_truncate(bool)` | `false` | Clip at the area edge instead of wrapping. |
| `set_selectable(bool)` | `true` | Allow selection. |
| `set_scroll_x(i32)` / `set_scroll_y(i32)` | `0` | Scroll offset (clamped ≥ 0). |

`WrapMode`:

```rust
pub enum WrapMode { None, Char, Word }
```

Word wrap keeps words intact, breaking only when a word doesn't fit;
`Char` breaks anywhere; `None` clips.

## Rendering behavior

`render_self` walks the chunks left-to-right, top-to-bottom:

- Newlines advance to the next row; spaces flush the current word and draw a
  space.
- Words that don't fit wrap to the next line (or are broken per `wrap_mode`).
- The attribute bitmask maps to ratatui modifiers (BOLD → `Modifier::BOLD`,
  UNDERLINE → `UNDERLINED`, INVERSE → `REVERSED`, BLINK → `SLOW_BLINK`, etc.).
- Alpha-0 colors render as `Color::Reset`.
- Wide graphemes (CJK, emoji) occupy their display columns and mark the
  continuation cells as skip (see [`unicode_util`](../lib/unicode_util.md)).

## Example

```rust,ignore
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::styled_text::{StyledText, TextChunk, string_to_styled_text};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::utils::{TextAttributeOptions, create_text_attributes};

let mut text = TextRenderable::new(Some(StyledText {
    chunks: vec![
        TextChunk { text: "done: ".into(), fg: None, bg: None, attributes: 0, link: None },
        TextChunk {
            text: "compiled".into(),
            fg: Some(RGBA::from_hex("#22c55e")),
            bg: None,
            attributes: create_text_attributes(TextAttributeOptions { bold: true, ..Default::default() }),
            link: None,
        },
    ],
}));
text.set_wrap_mode(cosh_tui::core::renderables::text::WrapMode::Word);

// text.render_self(&mut buf, area);
```

Next: [text_node — a tree of styled text](text_node.md).
