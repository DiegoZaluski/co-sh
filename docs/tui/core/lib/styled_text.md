# `styled_text` — styled strings

The model for text with inline styling. A `StyledText` is a list of
`TextChunk`s; each chunk is a run of text with its own foreground,
background, attribute bitmask, and optional link.

```rust
pub struct TextChunk {
    pub text: String,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,          // packed TextAttributes, see types.md
    pub link: Option<UrlLink>,
}

pub struct UrlLink {
    pub url: String,
}

pub struct StyledText {
    pub chunks: Vec<TextChunk>,
}
```

Why chunks? A paragraph rarely has one style — "the **bold** word" is three
runs (`the `, `bold`, ` word`). Chunks let you mix styles inline, exactly like
an editor token stream, and renderables like
[`TextRenderable`](../renderables/text.md) draw each chunk with its own
style.

## The `None` convention

`fg: None` / `bg: None` on a chunk means **inherit the renderable's default**.
Renderables resolve chunk colors as `chunk.fg.unwrap_or(default_fg)` — so a
chunk with `None` colors renders with the widget's configured colors, and a
chunk with `Some(rgba)` overrides them. Alpha 0 colors render as "reset"
(see [`rgba`](rgba.md)).

## Building a styled string

```rust
pub fn string_to_styled_text(content: &str) -> StyledText
```

Wraps the whole string in a single chunk with `fg: None`, `bg: None`,
`attributes: 0`, `link: None` — i.e. "render with defaults". This is what
`TextRenderable::new(None)` uses for its empty default, and the quick way to
get plain text into a styled widget.

To build richer text, construct chunks yourself:

```rust,ignore
use cosh_tui::core::lib::styled_text::{StyledText, TextChunk, UrlLink};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::types::TextAttributes;
use cosh_tui::core::utils::{TextAttributeOptions, create_text_attributes};

let styled = StyledText {
    chunks: vec![
        TextChunk {
            text: "hello ".into(),
            fg: None,
            bg: None,
            attributes: 0,
            link: None,
        },
        TextChunk {
            text: "world".into(),
            fg: Some(RGBA::from_hex("#ffcc00")),
            bg: None,
            attributes: create_text_attributes(TextAttributeOptions { bold: true, ..Default::default() }),
            link: Some(UrlLink { url: "https://example.com".into() }),
        },
    ],
};
```

## Related

- [`TextRenderable`](../renderables/text.md) — the widget that draws
  `StyledText` (accepts chunks with per-chunk colors/attributes/links).
- [`TextNodeRenderable`](../renderables/text_node.md) — a tree of styled
  text nodes that flattens to `Vec<TextChunk>` via inherited styles.
- [`detect_links`](detect_links.md) — annotates existing chunks with URLs.
- [`attributes_with_link`](../utils.md) — packs a link id into the attribute
  bits (a different, id-based link mechanism than `UrlLink`).

Next: [unicode_util — display widths and wrapping](unicode_util.md).
