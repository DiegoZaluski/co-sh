use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::renderable::Renderable;

static NEXT_SLOT_NUM: AtomicU64 = AtomicU64::new(1);

/// A renderable that rejects child mutations — it acts as a leaf placeholder.
struct SlotBaseRenderable {
    id: String,
    num: u64,
    visible: bool,
    destroyed: bool,
    parent_num: Option<u64>,
}

impl SlotBaseRenderable {
    fn new(prefix: &str) -> Self {
        let num = NEXT_SLOT_NUM.fetch_add(1, Ordering::Relaxed);
        SlotBaseRenderable {
            id: format!("{prefix}-{num}"),
            num,
            visible: false,
            destroyed: false,
            parent_num: None,
        }
    }
}

impl Renderable for SlotBaseRenderable {
    fn id(&self) -> &str { &self.id }
    fn num(&self) -> u64 { self.num }
    fn is_visible(&self) -> bool { self.visible }
    fn is_focusable(&self) -> bool { false }
    fn is_destroyed(&self) -> bool { self.destroyed }
    fn parent_num(&self) -> Option<u64> { self.parent_num }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
    fn children(&self) -> &[Box<dyn Renderable>] { &[] }
    fn render_self(&self, _buf: &mut Buffer, _area: Rect) {}
}

/// Text slot child that can be moved between parents without being destroyed.
///
/// When attached to a `SlotRenderable` parent, calling `detach_from_slot()`
/// clears the parent reference without destroying the node, allowing it to be
/// re-attached elsewhere.
pub struct TextSlotRenderable {
    base: SlotBaseRenderable,
    slot_parent_num: Option<u64>,
}

impl TextSlotRenderable {
    #[must_use]
    pub fn new() -> Self {
        TextSlotRenderable {
            base: SlotBaseRenderable::new("slot-text"),
            slot_parent_num: None,
        }
    }

    /// Detach from the owning `SlotRenderable` without destroying the node.
    pub fn detach_from_slot(&mut self) {
        self.slot_parent_num = None;
    }

    /// Dispose without cascading destruction to the slot parent.
    pub fn dispose_without_slot_cascade(&mut self) {
        if self.base.destroyed {
            return;
        }
        self.base.destroyed = true;
        self.detach_from_slot();
    }

    pub fn set_slot_parent(&mut self, parent_num: u64) {
        self.slot_parent_num = Some(parent_num);
    }

    #[must_use]
    pub fn slot_parent_num(&self) -> Option<u64> {
        self.slot_parent_num
    }

    /// Mark the node as destroyed, and if attached to a slot, destroy the slot.
    pub fn destroy_with_slot(&mut self) {
        if self.base.destroyed {
            return;
        }
        self.base.destroyed = true;
        // In a full implementation this would cascade to the slot parent.
        // For now we just detach.
        self.detach_from_slot();
    }
}

impl Default for TextSlotRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for TextSlotRenderable {
    fn id(&self) -> &str { self.base.id() }
    fn num(&self) -> u64 { self.base.num() }
    fn is_visible(&self) -> bool { self.base.is_visible() }
    fn is_focusable(&self) -> bool { false }
    fn is_destroyed(&self) -> bool { self.base.is_destroyed() }
    fn parent_num(&self) -> Option<u64> { self.base.parent_num() }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
    fn children(&self) -> &[Box<dyn Renderable>] { &[] }
    fn render_self(&self, _buf: &mut Buffer, _area: Rect) {}
}

/// A placeholder renderable that can host multiple slot children, routing
/// requests by parent.  Used by the reconciler to manage Portal-style
/// re-parenting.
///
/// Analogous to the TS `SlotRenderable` but without Yoga layout support.
pub struct SlotRenderable {
    base: SlotBaseRenderable,
    children_by_parent: HashMap<u64, Box<dyn Renderable>>,
}

impl SlotRenderable {
    #[must_use]
    pub fn new() -> Self {
        SlotRenderable {
            base: SlotBaseRenderable::new("slot"),
            children_by_parent: HashMap::new(),
        }
    }

    pub fn register_child(&mut self, parent_num: u64, child: Box<dyn Renderable>) {
        self.children_by_parent.insert(parent_num, child);
    }

    pub fn remove_child(&mut self, parent_num: u64) -> Option<Box<dyn Renderable>> {
        self.children_by_parent.remove(&parent_num)
    }

    #[must_use]
    pub fn get_child(&self, parent_num: u64) -> Option<&dyn Renderable> {
        self.children_by_parent.get(&parent_num).map(Box::as_ref)
    }

    /// Return the first attached child (determines the effective "parent" of the slot).
    #[must_use]
    pub fn current_child(&self) -> Option<&dyn Renderable> {
        self.children_by_parent.values().next().map(Box::as_ref)
    }

    pub fn clear(&mut self) {
        self.children_by_parent.clear();
    }

    #[must_use]
    pub fn child_count(&self) -> usize {
        self.children_by_parent.len()
    }
}

impl Default for SlotRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for SlotRenderable {
    fn id(&self) -> &str { self.base.id() }
    fn num(&self) -> u64 { self.base.num() }
    fn is_visible(&self) -> bool { self.base.is_visible() }
    fn is_focusable(&self) -> bool { false }
    fn is_destroyed(&self) -> bool { self.base.is_destroyed() }
    fn parent_num(&self) -> Option<u64> { self.base.parent_num() }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
    fn children(&self) -> &[Box<dyn Renderable>] { &[] }
    fn render_self(&self, _buf: &mut Buffer, _area: Rect) {}
}
