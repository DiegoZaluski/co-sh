# `markdown::context` — the nesting tracker

`MarkdownContext` is the state machine that tracks where you are in the
markdown while iterating a `pulldown_cmark` event stream. `MarkdownRenderable`
feeds it every `Start`/`End` event, then asks it "what element is current?",
"what heading level?", "am I inside a code block / blockquote / list?" to
pick the right style and layout.

```rust
MarkdownContext::new() -> Self
```

## `MarkdownElement`

The styled element categories:

```rust
pub enum MarkdownElement {
    Paragraph,
    Heading(u8),            // 1 = h1 … 6 = h6
    Emphasis,               // *italic*
    Strong,                 // **bold**
    Strikethrough,          // ~~text~~
    Link,                   // [label](url) — the label part
    InlineCode,             // `code`
    CodeBlock,              // fenced/indented block
    Blockquote,
    ListItem { ordered: bool },
}
```

## Feeding events

```rust
ctx.handle_start(&Tag<'_>);   // call on every Event::Start
ctx.handle_end(&TagEnd);      // call on every Event::End
```

## Querying state

| Method | Returns |
|---|---|
| `current_element() -> Option<MarkdownElement>` | The innermost open element. |
| `heading_level() -> Option<u8>` | Heading depth if inside a heading. |
| `in_code_block() -> bool` | Inside a fenced/indented code block. |
| `code_block_lang() -> &str` | The block's language tag (`""` if none). |
| `in_blockquote() -> bool` | Inside a blockquote. |
| `list_ordered() -> Option<bool>` | Whether the current list is ordered. |
| `list_marker() -> Option<(bool, usize)>` | `(ordered, number)` for the current list item. |

## Typical usage

```rust,ignore
use cosh_tui::core::renderables::markdown::context::{MarkdownContext, MarkdownElement};
use pulldown_cmark::{Parser, Options, Event, Tag, TagEnd};

let mut ctx = MarkdownContext::new();
let mut parser = Parser::new_ext(text, Options::empty());

for event in parser {
    match &event {
        Event::Start(tag) => ctx.handle_start(tag),
        Event::End(end)   => ctx.handle_end(end),
        Event::Text(_) => {
            let el = ctx.current_element();      // e.g. Some(MarkdownElement::Heading(1))
            let level = ctx.heading_level();
            // … style and render with el/level …
        }
        _ => {}
    }
}
```

This mirrors exactly how `MarkdownRenderable` renders, and is also what
`estimate_height` (see [layout](layout.md)) uses to count rows identically
to the renderer.
