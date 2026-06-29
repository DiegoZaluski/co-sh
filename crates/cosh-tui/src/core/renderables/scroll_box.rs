use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::renderable::Renderable;

static NEXT_SCROLL_BOX_NUM: AtomicU64 = AtomicU64::new(1);

pub struct ScrollBoxRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    scroll_x: i32,
    scroll_y: i32,
    content_width: i32,
    content_height: i32,
}

impl ScrollBoxRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_SCROLL_BOX_NUM.fetch_add(1, Ordering::Relaxed);
        ScrollBoxRenderable {
            id: format!("scrollbox-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            scroll_x: 0,
            scroll_y: 0,
            content_width: 0,
            content_height: 0,
        }
    }

    #[must_use]
    pub fn scroll_x(&self) -> i32 {
        self.scroll_x
    }

    pub fn set_scroll_x(&mut self, value: i32) {
        self.scroll_x = value.max(0);
    }

    #[must_use]
    pub fn scroll_y(&self) -> i32 {
        self.scroll_y
    }

    pub fn set_scroll_y(&mut self, value: i32) {
        self.scroll_y = value.max(0);
    }

    pub fn scroll_by(&mut self, dx: i32, dy: i32) {
        self.set_scroll_x(self.scroll_x + dx);
        self.set_scroll_y(self.scroll_y + dy);
    }

    pub fn set_content_size(&mut self, width: i32, height: i32) {
        self.content_width = width;
        self.content_height = height;
    }

    pub fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }

    pub fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }
}

impl Default for ScrollBoxRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for ScrollBoxRenderable {
    fn id(&self) -> &str {
        &self.id
    }

    fn num(&self) -> u64 {
        self.num
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focusable(&self) -> bool {
        self.focusable
    }

    fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    fn parent_num(&self) -> Option<u64> {
        self.parent_num
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        &self.children
    }

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        let _ = (buf, area);
    }
}
