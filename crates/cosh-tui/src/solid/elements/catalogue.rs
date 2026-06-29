use std::any::Any;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::renderable::Renderable;

static NEXT_CATALOGUE_NUM: AtomicU64 = AtomicU64::new(1);

/// A span of styled text with optional attributes (bold, italic, underline, etc.).
pub struct SpanRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    text: String,
    attributes: u32,
    link: Option<String>,
}

impl SpanRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_CATALOGUE_NUM.fetch_add(1, Ordering::Relaxed);
        SpanRenderable {
            id: format!("span-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            text: String::new(),
            attributes: 0,
            link: None,
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn set_attributes(&mut self, attrs: u32) {
        self.attributes = attrs;
    }

    pub fn set_link(&mut self, url: Option<String>) {
        self.link = url;
    }
}

impl Default for SpanRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for SpanRenderable {
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
        let mut modifier = Modifier::empty();
        if self.attributes & 1 != 0 {
            modifier |= Modifier::BOLD;
        }
        if self.attributes & 2 != 0 {
            modifier |= Modifier::ITALIC;
        }
        if self.attributes & 4 != 0 {
            modifier |= Modifier::UNDERLINED;
        }

        let color = if self.link.is_some() {
            Color::Rgb(66, 133, 244)
        } else {
            Color::Reset
        };

        let style = Style::default().fg(color).add_modifier(modifier);
        let mut x = area.x;

        for ch in self.text.chars() {
            if x >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            x += 1;
        }
    }
}

/// Bold text span.
pub type BoldSpanRenderable = SpanRenderable;

/// Italic text span.
pub type ItalicSpanRenderable = SpanRenderable;

/// Underlined text span.
pub type UnderlineSpanRenderable = SpanRenderable;

/// Line break renderable.
pub struct LineBreakRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,
}

impl LineBreakRenderable {
    #[must_use]
    pub fn new() -> Self {
        let num = NEXT_CATALOGUE_NUM.fetch_add(1, Ordering::Relaxed);
        LineBreakRenderable {
            id: format!("br-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
        }
    }
}

impl Default for LineBreakRenderable {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for LineBreakRenderable {
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
        if let Some(cell) = buf.cell_mut((area.x, area.y)) {
            cell.set_char('\n');
        }
    }
}

/// A link renderable — styled text with a URL.
pub struct LinkRenderable {
    inner: SpanRenderable,
}

impl LinkRenderable {
    #[must_use]
    pub fn new(url: String) -> Self {
        let mut inner = SpanRenderable::new();
        inner.set_link(Some(url));
        LinkRenderable { inner }
    }

    pub fn set_text(&mut self, text: String) {
        self.inner.set_text(text);
    }
}

impl Renderable for LinkRenderable {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn num(&self) -> u64 {
        self.inner.num()
    }
    fn is_visible(&self) -> bool {
        self.inner.is_visible()
    }
    fn is_focusable(&self) -> bool {
        self.inner.is_focusable()
    }
    fn is_destroyed(&self) -> bool {
        self.inner.is_destroyed()
    }
    fn parent_num(&self) -> Option<u64> {
        self.inner.parent_num()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn children(&self) -> &[Box<dyn Renderable>] {
        self.inner.children()
    }
    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        self.inner.add_child(child)
    }
    fn remove_child(&mut self, id: &str) {
        self.inner.remove_child(id);
    }
    fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        self.inner.insert_child_before(child, anchor_id)
    }

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        self.inner.render_self(buf, area);
    }
}

type ComponentConstructor = Box<dyn Fn() -> Box<dyn Renderable> + Send + Sync>;

static COMPONENT_REGISTRY: LazyLock<Mutex<HashMap<&'static str, ComponentConstructor>>> =
    LazyLock::new(|| {
        let mut m: HashMap<&'static str, ComponentConstructor> = HashMap::new();
        m.insert(
            "span",
            Box::new(|| -> Box<dyn Renderable> { Box::new(SpanRenderable::new()) })
                as ComponentConstructor,
        );
        m.insert(
            "br",
            Box::new(|| -> Box<dyn Renderable> { Box::new(LineBreakRenderable::new()) })
                as ComponentConstructor,
        );
        Mutex::new(m)
    });

pub fn register_component(name: &'static str, ctor: ComponentConstructor) {
    if let Ok(mut registry) = COMPONENT_REGISTRY.lock() {
        registry.insert(name, ctor);
    }
}

pub fn create_component(name: &str) -> Option<Box<dyn Renderable>> {
    if let Ok(registry) = COMPONENT_REGISTRY.lock() {
        registry.get(name).map(|ctor| ctor())
    } else {
        None
    }
}
