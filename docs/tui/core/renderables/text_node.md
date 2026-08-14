# `TextNodeRenderable` — a tree of styled text

A tree model for styled text. Unlike `TextRenderable` (a flat list of
chunks), `TextNodeRenderable` lets you build **nested** text: a root node
with child text nodes, each carrying its own style that **inherits** from its
parents (like CSS inheritance). Flattening the tree produces the
`Vec<TextChunk>` the renderers consume.

```rust
TextNodeRenderable::new(options: TextNodeOptions) -> Self
```

`TextNodeRenderable` does **not** implement `Renderable` — it's a data model
for text, not a widget. You flatten it to chunks and feed those to a
`TextRenderable`, or use it directly in logic that builds styled output.

## Options and children

```rust
pub struct TextNodeOptions {
    pub id: Option<String>,
    pub fg: Option<ColorInput>,
    pub bg: Option<ColorInput>,
    pub attributes: Option<u32>,
    pub link: Option<UrlLink>,
}

pub enum TextNodeChild {
    Text(String),               // a raw text leaf
    Node(Box<TextNodeRenderable>), // a nested styled node
}

pub enum TextNodeAddItem {
    Text(String),
    Node(TextNodeRenderable),
    StyledText(StyledText),     // expands into one node per chunk
}
```

## Building

| Method | Effect |
|---|---|
| `add(TextNodeAddItem) -> usize` | Append a child (index returned). `StyledText` adds one node per chunk. |
| `from_string(text, options)` | A node containing one text leaf. |
| `from_nodes(nodes, options)` | A root wrapping the given nodes. |
| `remove(id) -> Option<TextNodeChild>` | Remove a child node by id. |
| `clear()` | Drop all children. |

## Styling

| Method | Effect |
|---|---|
| `set_fg(Option<ColorInput>)` / `set_bg(..)` | Set this node's colors (marks dirty). |
| `set_attributes(u32)` | Set this node's attribute bitmask. |
| `set_link(Option<UrlLink>)` | Attach a URL. |

Each setter marks the node dirty; `is_dirty()` / `mark_clean()` let callers
skip re-flattening when nothing changed.

## Inheritance and flattening

```rust
pub struct InheritedStyle {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<UrlLink>,
}
// InheritedStyle::new() — all defaults

pub fn merge_styles(&self, parent_style: &InheritedStyle) -> InheritedStyle
pub fn gather_with_inherited_style(&mut self, parent_style: &InheritedStyle) -> Vec<TextChunk>
pub fn to_chunks(&mut self, parent_style: &InheritedStyle) -> Vec<TextChunk>
```

Inheritance rules in `merge_styles`:

- `fg` / `bg` / `link` — the node's value wins; `None` falls through to the
  parent's.
- `attributes` — **bitwise OR** of node and parent (attributes accumulate).

`gather_with_inherited_style` walks the tree depth-first: each text leaf
becomes a `TextChunk` styled with its effective inherited style, and each
nested node recurses with the merged style. The node is marked clean
afterward.

## Navigation

```rust
children() -> &[TextNodeChild]
children_mut() -> &mut Vec<TextNodeChild>
get_children() -> Vec<&Self>          // node children only
get_children_count() -> usize
get_renderable(id) -> Option<&Self>   // direct node child by id
get_renderable_index(id) -> Option<usize>
```

## Example

```rust,ignore
use cosh_tui::core::renderables::text_node::{
    TextNodeRenderable, TextNodeOptions, TextNodeAddItem, InheritedStyle,
};

let mut root = TextNodeRenderable::new(TextNodeOptions { ..Default::default() });
root.add(TextNodeAddItem::Text("Hello ".into()));

let mut bold = TextNodeRenderable::new(TextNodeOptions {
    attributes: Some(cosh_tui::core::types::TextAttributes::BOLD.bits()),
    ..Default::default()
});
bold.add(TextNodeAddItem::Text("world".into()));
root.add(TextNodeAddItem::Node(bold));

let chunks = root.to_chunks(&InheritedStyle::new());
assert_eq!(chunks.len(), 2);
assert_ne!(chunks[1].attributes & cosh_tui::core::types::TextAttributes::BOLD.bits(), 0);
```

Next: [markdown — the full markdown renderer](markdown/markdown.md).
