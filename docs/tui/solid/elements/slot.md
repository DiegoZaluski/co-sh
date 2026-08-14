# `elements::slot` — slot placeholder system

Slots are placeholders that can host children routed by their parent —
analogous to SolidJS portals / slot components. Two public renderables live
here, plus an internal leaf base.

## `SlotRenderable` — multi-parent placeholder

A placeholder that can host **multiple** children, keyed by the parent's
`num` — used by the reconciler for Portal-style re-parenting. It draws
nothing itself.

```rust
SlotRenderable::new() -> Self             // id "slot-{n}", invisible
```

| Method | Behavior |
|---|---|
| `register_child(parent_num, child)` | Attach a child under a parent key. |
| `remove_child(parent_num) -> Option<Box<dyn Renderable>>` | Detach and return it. |
| `get_child(parent_num) -> Option<&dyn Renderable>` | Look up by parent. |
| `current_child() -> Option<&dyn Renderable>` | The first attached child (the effective "parent"). |
| `clear()` | Drop all children. |
| `child_count() -> usize` | Number of attached children. |

Like `TextSlotRenderable`, it's a leaf for mutation purposes — `add_child`
is a no-op returning `0`, and `render_self` draws nothing.

## `TextSlotRenderable` — re-parentable text child

A slot child that can be **moved between parents without being destroyed**.
It's an invisible leaf renderable (`id "slot-text-{n}"`, not visible).

```rust
TextSlotRenderable::new() -> Self
```

| Method | Behavior |
|---|---|
| `set_slot_parent(parent_num)` / `slot_parent_num()` | Record which `SlotRenderable` owns it. |
| `detach_from_slot()` | Clear the slot-parent link **without** destroying the node — allows re-attachment elsewhere. |
| `dispose_without_slot_cascade()` | Mark destroyed and detach, without destroying the slot. |
| `destroy_with_slot()` | Mark destroyed and detach (future: cascades to the slot). |

The detach/dispose distinction is the point: a normal destroy would tear
down the parent; these methods let the node leave a slot cleanly and be
re-inserted, mirroring how text nodes are moved in a reactive tree.

## Internal `SlotBaseRenderable`

A private leaf placeholder that both public types embed: invisible,
non-focusable, no children, no drawing. It exists to share the "leaf
placeholder" bookkeeping.

## Example

```rust,ignore
use cosh_tui::solid::elements::slot::{SlotRenderable, TextSlotRenderable};
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::styled_text::string_to_styled_text;

let mut slot = SlotRenderable::new();
let mut text = TextSlotRenderable::new();
let parent_num = 42;
text.set_slot_parent(parent_num);
slot.register_child(parent_num, Box::new(text));

assert_eq!(slot.child_count(), 1);
assert!(slot.current_child().is_some());

// Move the text elsewhere: detach without destroying.
let mut detached = slot.remove_child(parent_num).expect("has a child");
// detached is the TextSlotRenderable; re-register it under a new parent…
slot.register_child(99, detached);
```

Next: [plugins — slot registry](../plugins/slot.md).
