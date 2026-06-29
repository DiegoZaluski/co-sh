use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

static NEXT_RENDERABLE_NUM: AtomicU64 = AtomicU64::new(1);

pub trait Renderable {
    fn id(&self) -> &str;
    fn num(&self) -> u64;
    fn is_visible(&self) -> bool;
    fn is_focusable(&self) -> bool;
    fn is_destroyed(&self) -> bool;
    fn parent_num(&self) -> Option<u64>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn render_self(&self, buf: &mut Buffer, area: Rect);
    fn children(&self) -> &[Box<dyn Renderable>];
    fn children_count(&self) -> usize {
        self.children().len()
    }
}

pub struct RenderableNode {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,
}

impl RenderableNode {
    pub fn new(id: Option<String>) -> Self {
        let num = NEXT_RENDERABLE_NUM.fetch_add(1, Ordering::Relaxed);
        RenderableNode {
            id: id.unwrap_or_else(|| format!("renderable-{num}")),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }

    pub fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }

    pub fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        let anchor_idx = self.children.iter().position(|c| c.id() == anchor_id)?;
        self.children.insert(anchor_idx, child);
        Some(anchor_idx)
    }
}

pub struct RootRenderable {
    node: RenderableNode,
}

impl RootRenderable {
    #[must_use]
    pub fn new() -> Self {
        RootRenderable {
            node: RenderableNode::new(Some("__root__".to_string())),
        }
    }

    pub fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        self.node.add_child(child)
    }

    pub fn remove_child(&mut self, id: &str) {
        self.node.remove_child(id);
    }

    pub fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        self.node.insert_child_before(child, anchor_id)
    }

    pub fn set_id(&mut self, id: String) {
        self.node.id = id;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.node.visible = visible;
    }

    pub fn set_focusable(&mut self, focusable: bool) {
        self.node.focusable = focusable;
    }
}

impl Renderable for RootRenderable {
    fn id(&self) -> &str {
        &self.node.id
    }

    fn num(&self) -> u64 {
        self.node.num
    }

    fn is_visible(&self) -> bool {
        self.node.visible
    }

    fn is_focusable(&self) -> bool {
        self.node.focusable
    }

    fn is_destroyed(&self) -> bool {
        self.node.destroyed
    }

    fn parent_num(&self) -> Option<u64> {
        self.node.parent_num
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        &self.node.children
    }

    fn render_self(&self, _buf: &mut Buffer, _area: Rect) {}

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Default for RootRenderable {
    fn default() -> Self {
        Self::new()
    }
}
