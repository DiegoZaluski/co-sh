# `renderable` — the trait and node model

Every element that can appear on screen implements the `Renderable` trait.
This page is the contract those widgets live by: what a renderable *is*, the
methods the framework calls, and the two ready-made implementations —
[`RenderableNode`](#renderablenode) and [`RootRenderable`](#rootrenderable) —
that most widgets build on.

```rust
pub trait Renderable {
    fn id(&self) -> &str;
    fn num(&self) -> u64;
    fn is_visible(&self) -> bool;
    fn is_focusable(&self) -> bool;
    fn is_destroyed(&self) -> bool;
    fn parent_num(&self) -> Option<u64>;
    fn set_parent_num(&mut self, parent_num: Option<u64>);
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn render_self(&self, buf: &mut Buffer, area: Rect);
    fn children(&self) -> &[Box<dyn Renderable>];
    fn children_mut(&mut self) -> &mut [Box<dyn Renderable>]; // default: &mut []
    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize;
    fn remove_child(&mut self, id: &str);
    fn insert_child_before(&mut self, child: Box<dyn Renderable>, anchor_id: &str) -> Option<usize>;
    // … defaulted hooks below …
}
```

---

## Identity: `id`, `num`, `parent_num`

- **`id()`** — a stable string identifier, unique among siblings, used by
  `remove_child`/`insert_child_before` to find a child. Widgets mint ids like
  `"box-1"`, `"text-2"`, `"md-3"` from an atomic counter.
- **`num()`** — a globally unique `u64` assigned at construction (also from an
  atomic counter). `num` is the *parent link* currency: `set_parent_num` /
  `parent_num` track which renderable adopted this one, so a widget can find
  its ancestors without holding references.
- **`parent_num()`** — `Some(num)` once adopted by another renderable, `None`
  for the root.

## Lifecycle flags

| Method | Meaning |
|---|---|
| `is_visible()` | Whether the widget should be drawn. The framework skips invisible subtrees. |
| `is_focusable()` | Whether the widget can receive focus. Interactive widgets (input, select, textarea) return `true`. |
| `is_destroyed()` | Whether the widget has been torn down. |

## Type erasure: `as_any` / `as_any_mut`

Because children are stored as `Box<dyn Renderable>`, callers that know a
widget's concrete type downcast through `Any`:

> **Why type erasure is needed:** The renderable tree needs to hold heterogeneous widget types (boxes, text, markdown, inputs, etc.) in a single collection. Rust's trait objects (`Box<dyn Renderable>`) provide this polymorphism while maintaining type safety. The `as_any` mechanism allows downcasting back to concrete types when needed (e.g., accessing widget-specific methods), balancing the flexibility of a mixed-type tree with the safety of Rust's type system.

```rust,ignore
let box_widget = widget.as_any().downcast_ref::<BoxRenderable>();
```

Every widget returns `self` for both.

## Drawing: `render_self` / `render`

`render_self(&self, buf, area)` is the core drawing method: write the widget's
visuals into the ratatui `Buffer` within `area` (a `Rect`). It takes `&self`
— the widget must be internally consistent at draw time.

`render(buf, area, delta_time)` is the per-frame wrapper the renderer calls:
by default it just forwards to `render_self`, but a widget can override it to
animate using `delta_time`.

## Children

```rust
fn children(&self) -> &[Box<dyn Renderable>];
fn children_mut(&mut self) -> &mut [Box<dyn Renderable>]; // default: &mut []
fn add_child(&mut self, child: Box<dyn Renderable>) -> usize;   // returns child index
fn remove_child(&mut self, id: &str);
fn insert_child_before(&mut self, child: Box<dyn Renderable>, anchor_id: &str) -> Option<usize>;
```

`add_child` returns the index the child was inserted at. `remove_child` removes
by `id`. `insert_child_before` inserts before the child whose `id()` is
`anchor_id`, returning the insertion index, or `None` if the anchor isn't a
child. The default `children_mut` returns an empty slice, so widgets that
don't hold children (like `TextRenderable`) need not override it — but any
widget that *does* hold children must, or layout and traversal will never see
them.

## Layout hooks

The managed renderer drives layout through these defaulted methods:

| Method | Purpose |
|---|---|
| `build_style() -> Option<taffy::Style>` | The widget's flexbox style. `None` means "no style contribution". `BoxRenderable` returns its border + gap style. |
| `layout_node() -> Option<taffy::NodeId>` / `set_layout_node(..)` | The widget's node id in the shared `LayoutTree`. |
| `apply_layout(&mut self, &taffy::Layout)` | Called with the computed position/size after layout, so the widget can store it. |

## Focus & interaction

- `focus()` / `blur()` — called by the framework when focus moves.
- `process_mouse_event(&mut self, event: &MouseEvent) -> bool` — handle a mouse
  event; return `true` if consumed (stops propagation).
- `request_render()` — ask the renderer to redraw.
- `opacity()` / `set_opacity(v)` — blend factor, default `1.0`.
- `z_index()` / `set_z_index(v)` — stacking order, default `0`.
- `live()` / `set_live(v)` — whether the widget wants continuous redraws.
- `on_update(delta_time)` — per-frame update hook.
- `on_resize(width, height)` — resize notification.
- `destroy()` — teardown.

---

## The free helpers: `adopt_child` / `adopt_child_before`

Widgets typically implement `add_child` by delegating to the shared helpers,
which set the parent link *and* push the child:

```rust
pub fn adopt_child(
    parent_num: u64,
    children: &mut Vec<Box<dyn Renderable>>,
    mut child: Box<dyn Renderable>,
) -> usize {
    child.set_parent_num(Some(parent_num));
    let idx = children.len();
    children.push(child);
    idx
}
```

`adopt_child_before` does the same but inserts before the child matching
`anchor_id`, returning `Option<usize>`.

## `RenderableNode`

A concrete, reusable implementation of the trait's bookkeeping. It stores the
identity, flags, parent link, children, and layout node — everything except
the drawing itself. Widgets can either embed a `RenderableNode` and delegate,
or replicate the pattern directly (most widgets in this crate replicate it,
since they add their own fields).

```rust
RenderableNode::new(id: Option<String>) -> Self   // auto-id "renderable-{n}" if None
node.add_child(child) -> usize
node.remove_child(id)
node.insert_child_before(child, anchor_id) -> Option<usize>
node.children_ref() -> &[Box<dyn Renderable>]
node.children_mut() -> &mut Vec<Box<dyn Renderable>>
node.layout_node() -> Option<taffy::NodeId>
node.set_layout_node(node)
```

## `RootRenderable`

The top of the tree. It's a `Renderable` whose `render_self` does nothing —
its job is to be the parent every top-level widget is attached to, so the
renderer has a single root to walk:

```rust
RootRenderable::new()                    // id "__root__"
root.add_child(boxed_widget) -> usize    // returns child index
root.remove_child(id)
root.insert_child_before(child, anchor_id) -> Option<usize>
root.set_id(String)
root.set_visible(bool)   // const
root.set_focusable(bool) // const
```

Usage with the managed renderer:

```rust,ignore
let mut renderer = Renderer::new(terminal, RendererConfig::default());
renderer.root_mut().add_child(Box::new(my_panel));
renderer.render_frame(0.0)?;
```

---

## Writing your own renderable

The minimal widget needs: an identity, the lifecycle flags, `as_any`/`as_any_mut`,
`render_self`, and child management. Everything else has a sensible default.
A tiny example — a label that draws "hello" at the area's top-left:

```rust,ignore
struct Label { id: String, num: u64 }

impl Renderable for Label {
    fn id(&self) -> &str { &self.id }
    fn num(&self) -> u64 { self.num }
    fn is_visible(&self) -> bool { true }
    fn is_focusable(&self) -> bool { false }
    fn is_destroyed(&self) -> bool { false }
    fn parent_num(&self) -> Option<u64> { None }
    fn set_parent_num(&mut self, _p: Option<u64>) {}
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        for (i, ch) in "hello".chars().enumerate() {
            if let Some(cell) = buf.cell_mut((area.x + i as u16, area.y)) {
                cell.set_char(ch);
            }
        }
    }
    fn children(&self) -> &[Box<dyn Renderable>] { &[] }
    fn add_child(&mut self, _c: Box<dyn Renderable>) -> usize { 0 }
    fn remove_child(&mut self, _id: &str) {}
    fn insert_child_before(&mut self, _c: Box<dyn Renderable>, _a: &str) -> Option<usize> { None }
}
```

In practice you rarely write a renderable from scratch — the
[`renderables`](renderables/renderables.md) library already covers text, boxes,
markdown, inputs, selects, scrollbars, tables, and more, and `RenderableNode`
handles the bookkeeping if you do.

Next: [types — attributes, mouse events, selection](types.md).

---

## Summary

- `Renderable` trait: every screen element implements it with identity (`id`, `num`, `parent_num`), lifecycle flags (`is_visible`, `is_focusable`, `is_destroyed`), type erasure (`as_any`/`as_any_mut`), drawing (`render_self`), and child management.
- Identity: `id()` is stable string identifier unique among siblings, `num()` is globally unique `u64` for parent-link currency, `parent_num()` tracks adoption.
- Lifecycle flags: `is_visible()` (whether to draw), `is_focusable()` (interactive widgets), `is_destroyed()` (torn down).
- Type erasure: children stored as `Box<dyn Renderable>`, downcast via `as_any().downcast_ref::<ConcreteType>()`.
- Drawing: `render_self(&self, buf, area)` writes visuals into ratatui `Buffer` within `area`; `render(buf, area, delta_time)` is per-frame wrapper for animation.
- Children: `add_child` returns insertion index, `remove_child` removes by id, `insert_child_before` inserts before anchor id; default `children_mut` returns empty slice.
- Layout hooks: `build_style()` (widget's flexbox style), `layout_node()`/`set_layout_node()` (taffy node id), `apply_layout()` (store computed position/size).
- Focus & interaction: `focus()`/`blur()`, `process_mouse_event()` (return true if consumed), `request_render()`, opacity/z-index/live flags, `on_update`/`on_resize`/`destroy` hooks.
- Free helpers: `adopt_child`/`adopt_child_before` set parent link and push child; widgets typically delegate to these.
- `RenderableNode`: concrete reusable implementation of bookkeeping (identity, flags, parent link, children, layout node); widgets can embed it or replicate the pattern.
- `RootRenderable`: top of tree with `render_self` that does nothing — parent for all top-level widgets so renderer has single root to walk.
