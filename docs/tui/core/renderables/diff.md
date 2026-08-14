# `DiffRenderable` — unified and split diffs

Renders a unified-diff text (as produced by `git diff` / `diff -u`) with
per-line coloring, line numbers, and a choice of **unified** or **split**
view. Use it to show file changes in the UI.

```rust
DiffRenderable::new(diff: Option<String>) -> Self
```

## View modes

```rust
pub enum DiffViewMode { Unified, Split }
// set_view_mode(DiffViewMode)
```

- **Unified** — one column; added lines on their own rows, removed lines on
  their own, context lines shared.
- **Split** — two side-by-side panels: removed lines on the left, added on
  the right, aligned into paired change blocks.

## Content

```rust
set_diff(String)   // replace the diff text
```

The widget parses standard unified diff syntax: `---`/`+++` file headers,
`@@` hunk headers (with `-old,count +new,count` positions), `+` adds,
`-` removes, and context lines.

## Colors

| Setter | Default | Effect |
|---|---|---|
| `set_added_bg` | `#1a4d1a` | Background of added lines. |
| `set_removed_bg` | `#4d1a1a` | Background of removed lines. |
| `set_context_bg` | transparent | Background of context lines. |
| `set_added_sign_color` | `#22c55e` | Foreground of `+` added lines. |
| `set_removed_sign_color` | `#ef4444` | Foreground of `-` removed lines. |
| `set_hunk_header_fg` | `#828bb8` | `@@` header color. |
| `set_line_number_fg` | `#888888` | Line number color. |
| `set_added_line_number_bg` / `set_removed_line_number_bg` | transparent | Line-number column backgrounds. |
| `set_show_line_numbers(bool)` | `true` | Toggle the line-number gutter. |

All color setters take `RGBA` directly (not `ColorInput`).

## Rendering behavior

- Added/removed lines get their background color and a `+`/`-` sign prefix.
- Hunk and file headers are colored distinctly.
- Line numbers are tracked from the `@@` hunk headers (old line numbers for
  removals/context, new for additions/context).
- In split view, panels are sized to fit the width, with a separator column,
  and line-number gutters sized to the largest number on each side.

## Example

```rust,ignore
use cosh_tui::core::renderables::diff::{DiffRenderable, DiffViewMode};

let diff = "\
--- a/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
 }
";

let mut view = DiffRenderable::new(Some(diff.into()));
view.set_view_mode(DiffViewMode::Unified);
// view.render_self(&mut buf, area);
```

Next: [text_table — bordered grid table](text_table.md).
