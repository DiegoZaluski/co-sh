# `markdown` — the full markdown renderer

`MarkdownRenderable` renders markdown source into a terminal area: headings,
emphasis, lists, blockquotes, inline code, fenced code blocks **with
tree-sitter syntax highlighting**, tables, task lists, links, and horizontal
rules. It is the largest and most capable widget in the library.

```rust
MarkdownRenderable::new(content: Option<String>) -> Self
```

It parses with `pulldown-cmark` (tables, task lists, and strikethrough
enabled), tracks nesting with a `MarkdownContext`, styles elements from a
theme-derived `MarkdownPalette`, and feeds the result into the buffer with
word wrapping and grapheme-aware layout.

## Configuration

| Setter | Default | Effect |
|---|---|---|
| `set_content(String)` | `""` | The markdown source. |
| `set_fg(Option<ColorInput>)` | light grey | Base foreground (drives the palette). |
| `set_bg(Option<ColorInput>)` | transparent | Base background (drives the palette). |
| `set_table_border_color(Option<ColorInput>)` | palette muted | Table border color override. |

Accessor: `content() -> &str`.

The fg/bg build a [`MarkdownPalette`](palette.md) that derives every element
color (headings, code blocks, blockquotes, list markers, …) from the base
colors.

## What it renders

| Markdown | Output |
|---|---|
| `#`–`######` headings | Bold text in the palette's heading color, level-sensitive. |
| `**bold**`, `*italic*`, `~~strike~~` | Corresponding ratatui modifiers. |
| Ordered / unordered lists | `1. ` / `• ` markers; task lists render `☑` / `☐`. |
| Blockquotes | Indented with a bar; `⚠`-prefixed quotes get a yellow warning box. |
| `` `code` `` | Inline code styling. |
| ```` ```lang ```` | Fenced block with a language label row and tree-sitter highlighting. |
| Tables | Grid table with borders, alternating row shading, header styling, column width fitting. |
| `---` | Horizontal rule. |
| `[label](url)` | Label (link styling). |
| Raw HTML | Rendered as muted text. |

### Code block highlighting

- ` ``` ` with no language → plain text, no highlighting.
- ` ```lang ` with a supported language → tree-sitter
  [`highlight`](../../../../sdk/tree_sitter/highlight.md) spans mapped to
  category colors (keyword, string, comment, type, function, number,
  builtin).
- Unknown language → falls back to JavaScript as a generic highlighter.

Highlight results are cached in a global LRU (1000 entries keyed by text +
language). Code blocks that render slowly (>500µs) log a rate-limited
`[PERF]` debug line (at most once per second).

## Height estimation

```rust
pub fn estimate_height(text: &str, max_w: u16) -> u16
```

Predicts the number of terminal rows `MarkdownRenderable` will need at width
`max_w`, following the **same** wrapping logic as rendering. Use it to size
a scroll container before drawing:

```rust,ignore
let h = estimate_height(markdown, area_width);
// … create a ScrollBoxRenderable with room for h rows …
```

## Plain-text extraction

```rust
pub fn markdown_to_visible_text(markdown: &str) -> String
```

Strips markdown syntax, returning the visible text — useful for previews or
copying content out of a rendered message.

## Example

```rust,ignore
use cosh_tui::core::renderables::markdown::MarkdownRenderable;

let mut md = MarkdownRenderable::new(Some(
    "# Title\n\nSome **bold** and `code`.\n\n```rust\nfn main() {}\n```\n".into(),
));
md.set_fg(Some("#e0e0e0".into()));
md.set_bg(Some("#101010".into()));

// md.render_self(&mut buf, area);
```

## Submodules

- [context](context.md) — `MarkdownContext` / `MarkdownElement` nesting tracker.
- [palette](palette.md) — `MarkdownPalette` element color derivation.
- [layout](layout.md) — `estimate_height` (also re-exported above).
