use taffy::{
    AvailableSpace, FlexDirection, LengthPercentage, NodeId, Rect, Size, Style, TaffyTree,
};

/// Wraps a `TaffyTree` and manages its root node.
pub struct LayoutTree {
    pub taffy: TaffyTree,
    pub root: NodeId,
}

impl LayoutTree {
    /// Create a layout tree with a root node.
    ///
    /// # Panics
    ///
    /// Panics if `taffy` cannot allocate the root node.
    #[must_use]
    pub fn new() -> Self {
        let mut taffy = TaffyTree::new();
        let root_style = Style::default();
        let root = taffy
            .new_leaf(root_style)
            .expect("failed to create root layout node");
        LayoutTree { taffy, root }
    }

    /// Create a new leaf node from a `Style`.
    ///
    /// # Panics
    ///
    /// Panics if `taffy` rejects the node style or cannot allocate the node.
    pub fn new_leaf(&mut self, style: Style) -> NodeId {
        self.taffy.new_leaf(style).expect("failed to create leaf")
    }

    /// Wrap a child in a new container node.
    ///
    /// # Panics
    ///
    /// Panics if any child node is invalid for this tree or `taffy` cannot
    /// allocate the container.
    pub fn new_container(&mut self, style: Style, children: &[NodeId]) -> NodeId {
        self.taffy
            .new_with_children(style, children)
            .expect("failed to create container")
    }

    /// Add an existing child to a parent.
    ///
    /// # Panics
    ///
    /// Panics if either node does not exist in this tree, or if `taffy` rejects
    /// the parent-child relationship.
    pub fn add_child(&mut self, parent: NodeId, child: NodeId) {
        self.taffy
            .add_child(parent, child)
            .expect("failed to add child");
    }

    /// Remove a child from a parent.
    ///
    /// # Panics
    ///
    /// Panics if either node does not exist in this tree, or if `child` is not a
    /// child of `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        self.taffy
            .remove_child(parent, child)
            .expect("failed to remove child");
    }

    /// Remove a node from the tree entirely.
    ///
    /// # Panics
    ///
    /// Panics if `node` does not exist in this tree.
    pub fn remove(&mut self, node: NodeId) {
        self.taffy.remove(node).expect("failed to remove node");
    }

    /// Compute layout for the entire tree starting from the root.
    ///
    /// # Panics
    ///
    /// Panics if `taffy` cannot compute layout for the current tree.
    pub fn compute_layout(&mut self, width: f32, height: f32) {
        let available = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
        self.taffy
            .compute_layout(self.root, available)
            .expect("layout computation failed");
    }

    /// Get computed layout for a node.
    ///
    /// # Panics
    ///
    /// Panics if `node` does not exist in this tree.
    #[must_use]
    pub fn layout(&self, node: NodeId) -> &taffy::Layout {
        self.taffy
            .layout(node)
            .expect("node not found in layout tree")
    }

    /// Set style on a node.
    ///
    /// # Panics
    ///
    /// Panics if `node` does not exist in this tree.
    pub fn set_style(&mut self, node: NodeId, style: Style) {
        self.taffy
            .set_style(node, style)
            .expect("failed to set style");
    }

    /// Mark a node as dirty (re-layout needed).
    ///
    /// # Panics
    ///
    /// Panics if `node` does not exist in this tree.
    pub fn mark_dirty(&mut self, node: NodeId) {
        self.taffy.mark_dirty(node).expect("failed to mark dirty");
    }
}

impl Default for LayoutTree {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Style helpers – convert common cosh-tui values into Taffy types.
// ---------------------------------------------------------------------------

/// Convert a border flag (on/off per side) to a Taffy `Rect<LengthPercentage>`.
/// Each active border side contributes 1.0 cell of border width.
#[allow(clippy::fn_params_excessive_bools)]
#[must_use]
pub fn border_rect(top: bool, right: bool, bottom: bool, left: bool) -> Rect<LengthPercentage> {
    Rect {
        left: if left {
            LengthPercentage::length(1.0)
        } else {
            LengthPercentage::length(0.0)
        },
        right: if right {
            LengthPercentage::length(1.0)
        } else {
            LengthPercentage::length(0.0)
        },
        top: if top {
            LengthPercentage::length(1.0)
        } else {
            LengthPercentage::length(0.0)
        },
        bottom: if bottom {
            LengthPercentage::length(1.0)
        } else {
            LengthPercentage::length(0.0)
        },
    }
}

/// Default flex container style used by `BoxRenderable`.
#[must_use]
pub fn default_box_style(border_rect: Rect<LengthPercentage>) -> Style {
    Style {
        display: taffy::Display::Flex,
        flex_direction: FlexDirection::Column,
        border: border_rect,
        ..Style::default()
    }
}
