# `markdown::layout` — height estimation

A single function that predicts how many terminal rows markdown will occupy,
so callers can size a scroll container before rendering.

```rust
pub fn estimate_height(text: &str, max_w: u16) -> u16
```

`estimate_height` runs the **same** `pulldown_cmark` event walk and the same
wrapping logic as [`MarkdownRenderable::render_self`](../markdown/markdown.md)
— it even mirrors the table-buffering behavior — so its answer matches what
the renderer will actually produce. List markers, code blocks, blockquotes,
and table rows are all counted identically.

- Empty text or `max_w == 0` returns `1`.
- Returns at least 1 row for any non-empty input.

## Example

```rust,ignore
use cosh_tui::core::renderables::markdown::layout::estimate_height;

let md = "# Title\n\nSome body text that may wrap across several lines.\n";
let rows = estimate_height(md, 40);   // how tall the rendered block will be
```

This is the module behind the `estimate_height` re-export on
[`markdown`](../markdown/markdown.md); the context tracker it uses is
documented in [context](context.md).
