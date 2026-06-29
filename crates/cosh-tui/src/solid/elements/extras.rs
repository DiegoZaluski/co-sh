use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::renderable::Renderable;

use super::catalogue::create_component;

static NEXT_DYNAMIC_NUM: AtomicU64 = AtomicU64::new(1);

/// A renderable that wraps another renderable resolved at construction time from
/// the component catalogue by name.
///
/// Analogous to `SolidJS`'s `<Dynamic>` component.  Unlike the TS original this
/// version does **not** support reactive prop spreading — props are set via
/// builder methods before the first render.
pub struct DynamicRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,
    inner: Option<Box<dyn Renderable>>,
}

impl DynamicRenderable {
    /// Try to resolve `component_name` from the catalogue and wrap it.
    /// Returns `None` when the name is not registered.
    #[must_use]
    pub fn try_new(component_name: &str) -> Option<Self> {
        let inner = create_component(component_name)?;
        let num = NEXT_DYNAMIC_NUM.fetch_add(1, Ordering::Relaxed);
        Some(DynamicRenderable {
            id: format!("dynamic-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            inner: Some(inner),
        })
    }

    /// Panic if the component name is not registered.
    ///
    /// # Panics
    ///
    /// Panics when `component_name` has not been registered in the component
    /// catalogue.
    #[must_use]
    pub fn new(component_name: &str) -> Self {
        Self::try_new(component_name)
            .unwrap_or_else(|| panic!("DynamicRenderable: unknown component `{component_name}`"))
    }
}

impl Renderable for DynamicRenderable {
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
    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }
    fn remove_child(&mut self, id: &str) {
        self.children.retain(|child| child.id() != id);
    }
    fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        let anchor_idx = self
            .children
            .iter()
            .position(|existing| existing.id() == anchor_id)?;
        self.children.insert(anchor_idx, child);
        Some(anchor_idx)
    }

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        if let Some(inner) = &self.inner {
            inner.render_self(buf, area);
        }
    }
}
