# `detect_links` — URL detection in highlighted text

Turns syntax-highlighter output into clickable text. Given the raw content, a
set of highlight ranges (with their token scopes), and the already-built
`TextChunk`s, `detect_links` finds URL ranges and stamps the matching chunks
with a `UrlLink`.

```rust
pub struct SimpleHighlight(pub usize, pub usize, pub String);  // (start, end, scope/group)

pub fn detect_links(
    chunks: Vec<TextChunk>,
    content: &str,
    highlights: &[SimpleHighlight],
) -> Vec<TextChunk>
```

## How it works

1. **Find URL ranges.** Highlights whose group is a URL scope
   (`"markup.link.url"` or `"string.special.url"`) become URL ranges. Each
   URL range also pulls in the immediately-preceding `"markup.link.label"`
   highlight (the visible link text) as a second range pointing at the same
   URL.
2. **Annotate chunks.** The chunks' text is located back in `content` (walking
   forward positionally); any chunk overlapping a URL range gets
   `chunk.link = Some(UrlLink { url })`.

Chunks whose text is a single character are skipped (too ambiguous to place),
and if no URL ranges were found the chunks are returned untouched.

```rust
pub fn detect_links(chunks: Vec<TextChunk>, content: &str, highlights: &[SimpleHighlight]) -> Vec<TextChunk>
```

## Example

```rust,ignore
use cosh_tui::core::lib::styled_text::{TextChunk, string_to_styled_text};
use cosh_tui::core::lib::detect_links::{detect_links, SimpleHighlight};

let content = "visit https://example.com now";
let chunks = string_to_styled_text(content).chunks;

let linked = detect_links(
    chunks,
    content,
    &[SimpleHighlight(6, 26, "markup.link.url".into())],
);

assert_eq!(linked[0].link.as_ref().map(|l| l.url.as_str()), Some("https://example.com"));
```

The label pull-in means a `[label](url)` markdown pair highlights both the
label and the URL as the same link target.

Next: [terminal_palette — OSC-4 palette detection](terminal_palette.md).
