# `reconciler` — DOM-node operations

Free functions that mutate a renderable tree the way a DOM API would —
create, insert, remove, and set properties on nodes. The "DOM" here is the
[`Renderable`](../core/renderable.md) tree, and every node is a
`Box<dyn Renderable>`:

```rust
pub type DomNode = Box<dyn Renderable>;
```

These are the operations a virtual-DOM reconciler (or a SolidJS-style
framework) would call. **Port status: partially implemented** — creation,
insertion, removal, and a few property setters work; several inspection
helpers are stubs.

## Creating nodes

| Function | Behavior |
|---|---|
| `create_text_node(value: &str) -> DomNode` | A `TextRenderable` containing the string (see [`core::renderables::text`](../core/renderables/text.md)). |
| `create_element(tag_name: &str) -> DomNode` | Resolve via [`create_component`](elements/catalogue.md); falls back to a `RootRenderable` for unknown tags. |
| `create_slot_node() -> DomNode` | An empty `RootRenderable` placeholder. |
| `TextNode::from_string(text, _options) -> DomNode` | Same as `create_text_node`. |

```rust
pub struct TextNode;   // namespace struct, no data
```

## Inserting and removing

```rust
pub fn insert_node(parent: &mut DomNode, node: &mut DomNode, anchor: Option<&str>)
pub fn remove_node(parent: &mut DomNode, node_id: &str)
```

`insert_node` attaches `node` to `parent` — before the child whose `id()`
equals `anchor` when given, otherwise appended. The node is moved out of its
old box (`std::mem::replace` with a fresh `RootRenderable`), so the caller's
variable is left as a harmless placeholder. `remove_node` detaches by id.

## Properties

```rust
pub fn set_property(node: &mut DomNode, name: &str, value: &str)
```

Sets a property by downcasting the node. Supported today:

| Name | Target | Value |
|---|---|---|
| `"id"` | `RootRenderable` | New id string. |
| `"visible"` | `RootRenderable` | `"true"` / `"false"`. |
| `"focusable"` | `RootRenderable` | `"true"` / `"false"`. |
| `"background"` | `BoxRenderable` | Color input string (see [`rgba`](../core/lib/rgba.md)). |
| `"border"` | `BoxRenderable` | `"true"` / `"false"`. |
| `"title"` | `BoxRenderable` | Title text. |

Unknown names are silently ignored.

## Stubs

These exist for API compatibility but return inert values until the port is
complete:

| Function | Current behavior |
|---|---|
| `replace_text(_node, _value)` | No-op. |
| `is_text_node(_node) -> bool` | Always `false`. |
| `get_parent_node(_node) -> Option<&DomNode>` | Always `None`. |
| `get_next_sibling(_node) -> Option<&DomNode>` | Always `None`. |
| `get_first_child(node)` | **Implemented** — the node's first child, or `None`. |

## Example

```rust,ignore
use cosh_tui::solid::reconciler::{
    create_text_node, create_element, insert_node, remove_node, set_property,
};
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderable::RootRenderable;

let mut root: Box<dyn Renderable> = Box::new(RootRenderable::new());

// A bordered panel via the property setter.
let mut panel = Box::new(BoxRenderable::new());
set_property(&mut panel, "border", "true");
set_property(&mut panel, "title", "output");

// Attach a text node.
let mut text = create_text_node("hello");
insert_node(&mut panel, &mut text, None);
insert_node(&mut root, &mut panel, None);

assert!(root.children().len() == 1);
remove_node(&mut root, root.children()[0].id());
```

Next: [renderer — the renderer traits](renderer/renderer.md).
