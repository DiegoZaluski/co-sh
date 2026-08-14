# `layout` — automatic layout with `LayoutTree`

`LayoutTree` is a thin wrapper around
[`taffy`](https://docs.rs/taffy) — the flexbox/grid layout engine — that owns
the tree's root node. It answers "where does each widget go and how big is
it?" given a fixed viewport, so the renderer can position widgets without
hand-placing every rectangle.

> **Why taffy for layout:** Manual layout calculation in terminal UIs is error-prone and tedious. Each widget needs to account for borders, padding, available space, and responsive resizing. Taffy brings the battle-tested flexbox/grid layout model from web development to terminal UIs, enabling declarative layout ("this panel should take 30% width, that one should fill remaining space") rather than imperative pixel/position math. This makes UI code more maintainable and responsive to terminal size changes.

```rust
pub struct LayoutTree {
    pub taffy: TaffyTree,
    pub root: NodeId,
}
```

You usually don't construct or drive a `LayoutTree` yourself — the
[`Renderer`](renderer.md) builds one per frame from the renderable tree. But
the type is public, so you can also lay out a custom tree directly.

---

## Building a tree

| Method | Purpose |
|---|---|
| `new()` | Create a tree with a default root node. |
| `new_leaf(style) -> NodeId` | Create a leaf node from a taffy `Style`. |
| `new_container(style, children: &[NodeId]) -> NodeId` | Wrap existing children in a new container. |
| `add_child(parent, child)` | Attach an existing child to a parent. |
| `remove_child(parent, child)` | Detach a child. |
| `remove(node)` | Remove a node from the tree entirely. |
| `set_style(node, style)` | Update a node's style. |
| `mark_dirty(node)` | Mark a node as needing re-layout. |

Every method **panics** if given a node that doesn't exist in the tree (or an
invalid parent/child relationship), and `new_leaf`/`new_container`/`compute_layout`
panic if taffy rejects the request. Node ids are opaque `taffy::NodeId`
values — keep the ones `new_leaf`/`new_container` return.

## Computing and reading layout

```rust
compute_layout(&mut self, width: f32, height: f32)   // solve the whole tree at a fixed size
layout(&self, node: NodeId) -> &taffy::Layout        // read a node's solved position/size
```

`compute_layout` sizes the tree against a *definite* `width`/`height` (the
viewport). After solving, `layout(node)` returns taffy's computed `Layout`
(`location` + `size`), which the renderer feeds back into each renderable via
[`Renderable::apply_layout`](renderable.md).

## Style helpers

Two free functions bridge cosh-tui values into taffy styles:

```rust
pub const fn border_rect(
    top: bool, right: bool, bottom: bool, left: bool,
) -> Rect<LengthPercentage>
```

Converts four per-side border flags into a taffy border rect — each active
side contributes **1.0 cell** of border width. Used by `BoxRenderable` so its
border takes up layout space.

```rust
pub fn default_box_style(border_rect: Rect<LengthPercentage>) -> Style
```

The default flex container style used by `BoxRenderable`: `display: flex`,
`flex_direction: column`, with the given border.

---

## Example — laying out two stacked panels

```rust,ignore
use cosh_tui::core::layout::LayoutTree;
use taffy::{Style, LengthPercentage};

let mut tree = LayoutTree::new();

let top = tree.new_leaf(Style::default());
let bottom = tree.new_leaf(Style::default());
let col = tree.new_container(
    Style {
        flex_direction: taffy::FlexDirection::Column,
        gap: taffy::Size { width: LengthPercentage::length(1.0), height: LengthPercentage::length(1.0) },
        ..Style::default()
    },
    &[top, bottom],
);
tree.add_child(tree.root, col);

tree.compute_layout(80.0, 24.0);
let top_layout = tree.layout(top);      // read solved position/size
println!("top at {:?} size {:?}", top_layout.location, top_layout.size);
```

Next: [renderer — driving the frame loop](renderer.md).

---

## Summary

- `LayoutTree` is a thin wrapper around taffy (flexbox/grid layout engine) that owns the tree's root node and answers "where does each widget go and how big is it?".
- Building methods: `new()` (default root), `new_leaf(style)` (leaf node), `new_container(style, children)` (wrap existing children), `add_child`/`remove_child`/`remove` (tree manipulation), `set_style`/`mark_dirty` (style updates).
- Computing and reading: `compute_layout(width, height)` solves the whole tree at a fixed size (viewport), `layout(node)` reads a node's solved position/size (taffy's `Layout` with location + size).
- Style helpers: `border_rect` converts four border flags into taffy border rect (each active side contributes 1.0 cell), `default_box_style` is the default flex container style used by `BoxRenderable`.
- Every method panics if given a non-existent node or invalid parent/child relationship; node ids are opaque `taffy::NodeId` values from `new_leaf`/`new_container`.
- Usually not constructed directly — the `Renderer` builds one per frame from the renderable tree, but the type is public for custom tree layout.
