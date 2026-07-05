use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::types::MouseEvent;

static NEXT_RENDERABLE_NUM: AtomicU64 = AtomicU64::new(1);

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
    fn children_mut(&mut self) -> &mut [Box<dyn Renderable>] {
        &mut []
    }
    fn children_count(&self) -> usize {
        self.children().len()
    }

    fn request_render(&mut self) {}
    fn focus(&mut self) {}
    fn blur(&mut self) {}
    fn opacity(&self) -> f32 {
        1.0
    }
    fn set_opacity(&mut self, _v: f32) {}
    fn z_index(&self) -> i32 {
        0
    }
    fn set_z_index(&mut self, _v: i32) {}
    fn live(&self) -> bool {
        false
    }
    fn set_live(&mut self, _v: bool) {}
    fn render(&self, buf: &mut Buffer, area: Rect, _delta_time: f64) {
        self.render_self(buf, area);
    }
    fn on_update(&mut self, _delta_time: f64) {}
    fn on_resize(&mut self, _width: i32, _height: i32) {}
    fn destroy(&mut self) {}

    fn layout_node(&self) -> Option<taffy::NodeId> {
        None
    }
    fn set_layout_node(&mut self, _node: Option<taffy::NodeId>) {}
    fn build_style(&self) -> Option<taffy::Style> {
        None
    }
    fn apply_layout(&mut self, _layout: &taffy::Layout) {}

    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize;
    fn remove_child(&mut self, id: &str);
    fn insert_child_before(&mut self, child: Box<dyn Renderable>, anchor_id: &str)
    -> Option<usize>;

    /// Process a mouse event targeting this renderable.
    /// Returns `true` if the event was handled.
    fn process_mouse_event(&mut self, _event: &MouseEvent) -> bool {
        false
    }
}

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

pub fn adopt_child_before(
    parent_num: u64,
    children: &mut Vec<Box<dyn Renderable>>,
    mut child: Box<dyn Renderable>,
    anchor_id: &str,
) -> Option<usize> {
    let anchor_idx = children.iter().position(|c| c.id() == anchor_id)?;
    child.set_parent_num(Some(parent_num));
    children.insert(anchor_idx, child);
    Some(anchor_idx)
}

pub struct RenderableNode {
    pub id: String,
    pub num: u64,
    pub visible: bool,
    pub focusable: bool,
    pub destroyed: bool,
    pub parent_num: Option<u64>,
    pub children: Vec<Box<dyn Renderable>>,
    pub layout_node: Option<taffy::NodeId>,
}

impl RenderableNode {
    #[must_use]
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
            layout_node: None,
        }
    }

    pub fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        adopt_child(self.num, &mut self.children, child)
    }

    pub fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }

    pub fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        adopt_child_before(self.num, &mut self.children, child, anchor_id)
    }

    #[must_use]
    pub fn children_ref(&self) -> &[Box<dyn Renderable>] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut Vec<Box<dyn Renderable>> {
        &mut self.children
    }

    #[must_use]
    pub fn layout_node(&self) -> Option<taffy::NodeId> {
        self.layout_node
    }

    pub fn set_layout_node(&mut self, node: Option<taffy::NodeId>) {
        self.layout_node = node;
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

    fn set_parent_num(&mut self, parent_num: Option<u64>) {
        self.node.parent_num = parent_num;
    }

    fn layout_node(&self) -> Option<taffy::NodeId> {
        self.node.layout_node()
    }

    fn set_layout_node(&mut self, node: Option<taffy::NodeId>) {
        self.node.set_layout_node(node);
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        self.node.children_ref()
    }

    fn render_self(&self, _buf: &mut Buffer, _area: Rect) {}

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        self.node.add_child(child)
    }

    fn remove_child(&mut self, id: &str) {
        self.node.remove_child(id);
    }

    fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        self.node.insert_child_before(child, anchor_id)
    }
}

impl Default for RootRenderable {
    fn default() -> Self {
        Self::new()
    }
}
