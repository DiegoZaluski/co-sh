# `markdown::palette` — element color derivation

`MarkdownPalette` derives the color for every markdown element from two base
colors — the text color and the background — so a markdown block looks
cohesive in any theme.

```rust
MarkdownPalette::new(text: RGBA, background: RGBA) -> Self
```

The palette is created internally by `MarkdownRenderable` from its `fg`/`bg`
settings (see [markdown](markdown.md)); you normally don't construct it
yourself, but the type is public for custom renderers that want the same
color logic.

## Color accessors

| Method | Returns |
|---|---|
| `text_color() -> RGBA` | Base text color (the `text` passed to `new`). |
| `background_color() -> RGBA` | Base background (the `background` passed to `new`). |
| `muted_color() -> RGBA` | Dimmed variant for secondary text (metadata, HTML, rules). |
| `code_block_bg() -> RGBA` | Code block background. |
| `blockquote_bg_color() -> RGBA` | Blockquote background. |
| `blockquote_bar_color() -> RGBA` | Blockquote bar color. |
| `list_marker_color() -> RGBA` | List bullet / number color. |

## Styling an element

```rust
pub fn style_for(&self, element: Option<MarkdownElement>, heading_level: Option<u8>) -> Style
```

Returns a ratatui `Style` for the given [`MarkdownElement`](context.md)
(and heading level): headings get bold + a heading color, `Strong` gets the
BOLD modifier, `Emphasis` ITALIC, `Strikethrough` CROSSED_OUT, code elements
get the code background, and so on. `None` (or an unknown element) yields
the base text style.

Two ratatui-`Color` helpers:

```rust
pub const fn code_bg_color() -> Color
pub const fn quote_bg_color() -> Color
pub const fn bg_color() -> Color
pub const fn rgba_to_ratatui(c: RGBA) -> Color   // RGBA → Color::Rgb (alpha 0 → Reset)
```

## Example

```rust,ignore
use cosh_tui::core::renderables::markdown::MarkdownPalette;
use cosh_tui::core::renderables::markdown::context::MarkdownElement;
use cosh_tui::core::lib::rgba::RGBA;

let palette = MarkdownPalette::new(
    RGBA::from_hex("#e0e0e0"),
    RGBA::from_hex("#101010"),
);

let heading_style = palette.style_for(Some(MarkdownElement::Heading(1)), Some(1));
let code_style = palette.style_for(Some(MarkdownElement::CodeBlock), None);
```
