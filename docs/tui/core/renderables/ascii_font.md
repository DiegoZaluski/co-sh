# `ASCIIFontRenderable` — ASCII-art font

Renders text using an ASCII-art font (default `"tiny"`), one character per
column in the area's top row. Use it for decorative headers or logos.

```rust
ASCIIFontRenderable::new(text: Option<String>, font: Option<String>, color: Option<ColorInput>) -> Self
```

| Parameter | Default |
|---|---|
| `text` | `""` |
| `font` | `"tiny"` |
| `color` | white |

## Configuration

```rust
set_text(String)
set_font(String)
set_color(ColorInput)           // note: ColorInput, not Option
set_background_color(ColorInput)
```

## Rendering

`render_self` draws each character of the text at increasing x positions on
the area's first row, in the configured color (or background color), clipped
to the area width. The `font` name is stored for future rendering — the
current implementation draws characters one-per-cell rather than expanding
to a multi-row bitmap font.

## Example

```rust,ignore
use cosh_tui::core::renderables::ascii_font::ASCIIFontRenderable;

let mut logo = ASCIIFontRenderable::new(
    Some("COSH".into()),
    Some("tiny".into()),
    Some("#22c55e".into()),
);
// logo.render_self(&mut buf, area);
```

Back to [renderables — the widget library](renderables.md), or
[core — module overview](../core.md).
