# `CodeRenderable` — syntax-highlighted code

Renders a code string with optional syntax highlighting (via a
[`SyntaxStyle`](../syntax_style.md) registry), word wrapping, and dimmed
styling. Use it for code listings inside the UI.

```rust
CodeRenderable::new(content: Option<String>, filetype: Option<String>) -> Self
```

## Configuration

| Method | Default | Effect |
|---|---|---|
| `set_content(String)` | `""` | The code text. |
| `set_filetype(String)` | `""` | Language hint (currently advisory). |
| `set_syntax_style(Option<SyntaxStyle>)` | `None` | Attach a style registry (see [`syntax_style`](../syntax_style.md)). |
| `set_fg(Option<ColorInput>)` | `#c8c8c8` | Text color. |
| `set_bg(Option<ColorInput>)` | transparent | Background. |
| `set_conceal(bool)` | `true` | Reserved conceal behavior. |
| `set_wrap_mode(WrapMode)` | `Word` | `None` \| `Char` \| `Word` wrapping. |

```rust
pub enum WrapMode { None, Char, Word }
```

## Rendering

`render_self` splits the content into lines, wraps each to the area width
(word wrap), and draws grapheme by grapheme in a **dimmed** style derived
from `fg`/`bg`. Alpha-0 colors render as `Color::Reset`. Wide graphemes
(CJK/emoji) consume their display columns.

Note: the current implementation renders with a single dimmed style — the
`SyntaxStyle` registry is available for future per-token highlighting but is
not yet applied per token by this widget. For rich, per-element colored code
blocks inside markdown, use
[`MarkdownRenderable`](markdown/markdown.md), which does full tree-sitter
highlighting.

## Example

```rust,ignore
use cosh_tui::core::renderables::code::CodeRenderable;

let mut code = CodeRenderable::new(
    Some("fn main() {\n    println!(\"hi\");\n}".into()),
    Some("rust".into()),
);
code.set_fg(Some("#d0d0d0".into()));
code.set_bg(Some("#1e1e1e".into()));

// code.render_self(&mut buf, area);
```

Next: [diff — unified and split diffs](diff.md).
