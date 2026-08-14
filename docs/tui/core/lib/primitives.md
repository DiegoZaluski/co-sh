# `lib` — the shared building blocks

The `lib` module collects the low-level primitives that widgets, the
renderer, and applications all share. Each one is a small, focused module:

| Source file | Page | Contents |
|---|---|---|
| `lib/rgba.rs` | [rgba](rgba.md) | `RGBA`, `ColorInput`, color parsing (hex, CSS names, ANSI-256), HSV. |
| `lib/border.rs` | [border](border.md) | `BorderCharacters`, `BorderStyle`, border presets, side configs. |
| `lib/styled_text.rs` | [styled_text](styled_text.md) | `StyledText`, `TextChunk`, `UrlLink`. |
| `lib/unicode_util.rs` | [unicode_util](unicode_util.md) | Grapheme display widths, `word_wrap`. |
| `lib/detect_links.rs` | [detect_links](detect_links.md) | URL detection in highlighted text. |
| `lib/terminal_palette.rs` | [terminal_palette](terminal_palette.md) | OSC-4 terminal palette detection. |

These types appear everywhere in the core API — a `TextChunk` carries
`RGBA` colors and a `u32` attribute mask; a `BoxRenderable` draws its border
from `BorderCharacters`; the markdown and code renderables rely on
`unicode_util` to lay out graphemes and on `rgba` to interpret color inputs.

Start with [rgba](rgba.md) (colors) and [unicode_util](unicode_util.md)
(widths) — nearly every renderable touches both.
