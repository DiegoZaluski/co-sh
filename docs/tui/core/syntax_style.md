# `syntax_style` — the style registry

`SyntaxStyle` is a registry of named styles used for syntax highlighting and
token theming. You register styles under names like `"keyword"` or
`"markup.link.url"`, then merge several names together to get the combined
`fg` / `bg` / `attributes` for a token. It powers the `CodeRenderable` widget
and is a lighter-weight alternative to driving `StyleDefinition`s by hand.

```rust
let mut styles = SyntaxStyle::create();
```

---

## The data types

```rust
pub struct StyleDefinition {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

pub struct StyleDefinitionInput {
    pub fg: Option<ColorInput>,   // accepts strings like "#ffcc00"
    pub bg: Option<ColorInput>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

pub struct MergedStyle {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,          // packed TextAttributes, see types.md
}

pub struct ThemeTokenStyle {
    pub scope: Vec<String>,       // e.g. ["keyword.control", "keyword"]
    pub style: ThemeTokenStyleInner,
}
pub struct ThemeTokenStyleInner {
    pub foreground: Option<ColorInput>,
    pub background: Option<ColorInput>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}
```

## Constructing

| Constructor | Source of styles |
|---|---|
| `create()` | Empty registry. |
| `from_styles(&HashMap<String, StyleDefinitionInput>)` | A flat name → style map. |
| `from_theme(&[ThemeTokenStyle])` | A theme token list (each token may list several scopes). |

`convert_theme_to_styles(theme: &[ThemeTokenStyle]) -> HashMap<String, StyleDefinition>`
is the underlying free function that flattens a theme: every scope in every
token becomes a key.

## Registering and resolving

```rust
styles.register_style("keyword", &StyleDefinitionInput { fg: Some("#ffb464".into()), ..Default::default() }) -> u64
styles.resolve_style_id("keyword") -> Option<u64>   // exact id, or None
styles.get_style_id("keyword.control") -> Option<u64> // falls back to base name before '.'
styles.get_style("keyword.control") -> Option<&StyleDefinition>
styles.get_style_count() -> usize
styles.get_all_styles() -> HashMap<String, StyleDefinition>
styles.get_registered_names() -> Vec<String>
```

Scoped lookups fall back to the **base name**: `get_style("keyword.control")`
returns the style registered under `"keyword"` if no exact match exists. This
mirrors how editor token scopes inherit from parent scopes.

## Merging

```rust
let merged: MergedStyle = styles.merge_styles(&["keyword", "keyword.control"]);
```

`merge_styles` overlays the named styles in order — later names override
earlier ones field-by-field — packs the resulting booleans into the
`attributes` bitmask via [`create_text_attributes`](utils.md), and **caches**
the result keyed by the joined name list (`"keyword:keyword.control"`), so
repeated merges of the same set are cheap.

Cache management: `clear_cache()` empties the merge cache,
`get_cache_size()` reports its size, `clear_name_cache()` drops the name → id
map (ids stay valid), and `destroy()` frees everything.

## Lifecycle guard

All methods assert the registry is not destroyed (`SyntaxStyle::destroy()`).
Calling any method after `destroy()` panics.

---

## Example — themed tokens for a code widget

```rust,ignore
use cosh_tui::core::syntax_style::{SyntaxStyle, StyleDefinitionInput, ThemeTokenStyle, ThemeTokenStyleInner};

let theme = vec![
    ThemeTokenStyle {
        scope: vec!["keyword".into(), "keyword.control".into()],
        style: ThemeTokenStyleInner { foreground: Some("#ffb464".into()), ..Default::default() },
    },
    ThemeTokenStyle {
        scope: vec!["string".into()],
        style: ThemeTokenStyleInner { foreground: Some("#96c896".into()), ..Default::default() },
    },
];
let styles = SyntaxStyle::from_theme(&theme);

let merged = styles.merge_styles(&["keyword.control"]);  // inherits "keyword"
// merged.fg == Some(rgba(1.0, 0.71, 0.39, 1.0)), merged.attributes == 0
```

Next: [utils — text attribute helpers](utils.md).
