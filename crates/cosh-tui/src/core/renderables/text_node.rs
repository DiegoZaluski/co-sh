use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::rgba::{parse_color, ColorInput, RGBA};
use crate::core::text::{StyledText, TextChunk};

static NEXT_TEXT_NODE_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct UrlLink {
    pub url: String,
}

#[derive(Debug, Clone)]
#[derive(Default)]
pub struct TextNodeOptions {
    pub id: Option<String>,
    pub fg: Option<ColorInput>,
    pub bg: Option<ColorInput>,
    pub attributes: Option<u32>,
    pub link: Option<UrlLink>,
}


#[derive(Debug, Clone)]
pub enum TextNodeChild {
    Text(String),
    Node(Box<TextNodeRenderable>),
}

#[derive(Debug, Clone)]
pub struct TextNodeRenderable {
    id: String,
    num: u64,
    fg: Option<RGBA>,
    bg: Option<RGBA>,
    attributes: u32,
    link: Option<UrlLink>,
    children: Vec<TextNodeChild>,
    dirty: bool,
}

impl TextNodeRenderable {
    pub fn new(options: TextNodeOptions) -> Self {
        let num = NEXT_TEXT_NODE_NUM.fetch_add(1, Ordering::Relaxed);
        TextNodeRenderable {
            id: options.id.unwrap_or_else(|| format!("textnode-{num}")),
            num,
            fg: options.fg.map(parse_color),
            bg: options.bg.map(parse_color),
            attributes: options.attributes.unwrap_or(0),
            link: options.link,
            children: Vec::new(),
            dirty: false,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn num(&self) -> u64 {
        self.num
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    #[must_use]
    pub fn children(&self) -> &[TextNodeChild] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut Vec<TextNodeChild> {
        &mut self.children
    }

    #[must_use]
    pub fn fg(&self) -> Option<RGBA> {
        self.fg
    }

    pub fn set_fg(&mut self, fg: Option<ColorInput>) {
        self.fg = fg.map(parse_color);
        self.mark_dirty();
    }

    #[must_use]
    pub fn bg(&self) -> Option<RGBA> {
        self.bg
    }

    pub fn set_bg(&mut self, bg: Option<ColorInput>) {
        self.bg = bg.map(parse_color);
        self.mark_dirty();
    }

    #[must_use]
    pub fn attributes(&self) -> u32 {
        self.attributes
    }

    pub fn set_attributes(&mut self, attributes: u32) {
        self.attributes = attributes;
        self.mark_dirty();
    }

    #[must_use]
    pub fn link(&self) -> Option<&UrlLink> {
        self.link.as_ref()
    }

    pub fn set_link(&mut self, link: Option<UrlLink>) {
        self.link = link;
        self.mark_dirty();
    }

    pub fn add(&mut self, obj: TextNodeAddItem) -> usize {
        match obj {
            TextNodeAddItem::Text(text) => {
                let idx = self.children.len();
                self.children.push(TextNodeChild::Text(text));
                self.mark_dirty();
                idx
            }
            TextNodeAddItem::Node(node) => {
                let idx = self.children.len();
                self.children.push(TextNodeChild::Node(Box::new(node)));
                self.mark_dirty();
                idx
            }
            TextNodeAddItem::StyledText(st) => {
                let nodes = styled_text_to_text_nodes(&st);
                let idx = self.children.len();
                for node in nodes {
                    self.children.push(TextNodeChild::Node(Box::new(node)));
                }
                self.mark_dirty();
                idx
            }
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<TextNodeChild> {
        if let Some(idx) = self
            .children
            .iter()
            .position(|c| matches!(c, TextNodeChild::Node(n) if n.id() == id))
        {
            let removed = self.children.remove(idx);
            self.mark_dirty();
            Some(removed)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
        self.mark_dirty();
    }

    #[must_use]
    pub fn merge_styles(
        &self,
        parent_style: &InheritedStyle,
    ) -> InheritedStyle {
        InheritedStyle {
            fg: self.fg.or(parent_style.fg),
            bg: self.bg.or(parent_style.bg),
            attributes: self.attributes | parent_style.attributes,
            link: self.link.clone().or_else(|| parent_style.link.clone()),
        }
    }

    pub fn gather_with_inherited_style(
        &mut self,
        parent_style: &InheritedStyle,
    ) -> Vec<TextChunk> {
        let current_style = self.merge_styles(parent_style);
        let mut chunks = Vec::new();

        for child in &mut self.children {
            match child {
                TextNodeChild::Text(text) => {
                    chunks.push(TextChunk {
                        text: text.clone(),
                        fg: current_style.fg,
                        bg: current_style.bg,
                        attributes: current_style.attributes,
                    });
                }
                TextNodeChild::Node(node) => {
                    let child_chunks = node.gather_with_inherited_style(&current_style);
                    chunks.extend(child_chunks);
                }
            }
        }

        self.mark_clean();
        chunks
    }

    #[must_use]
    pub fn get_children(&self) -> Vec<&TextNodeRenderable> {
        self.children
            .iter()
            .filter_map(|c| {
                if let TextNodeChild::Node(n) = c {
                    Some(n.as_ref())
                } else {
                    None
                }
            })
            .collect()
    }

    #[must_use]
    pub fn get_children_count(&self) -> usize {
        self.children.len()
    }

    #[must_use]
    pub fn get_renderable(&self, id: &str) -> Option<&TextNodeRenderable> {
        self.children.iter().find_map(|c| {
            if let TextNodeChild::Node(n) = c {
                if n.id() == id {
                    Some(n.as_ref())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn get_renderable_index(&self, id: &str) -> Option<usize> {
        self.children.iter().position(|c| {
            matches!(c, TextNodeChild::Node(n) if n.id() == id)
        })
    }

    #[must_use]
    pub fn from_string(text: &str, options: TextNodeOptions) -> Self {
        let mut node = TextNodeRenderable::new(options);
        node.add(TextNodeAddItem::Text(text.to_string()));
        node
    }

    #[must_use]
    pub fn from_nodes(nodes: Vec<TextNodeRenderable>, options: TextNodeOptions) -> Self {
        let mut root = TextNodeRenderable::new(options);
        for node in nodes {
            root.add(TextNodeAddItem::Node(node));
        }
        root
    }

    pub fn to_chunks(&mut self, parent_style: &InheritedStyle) -> Vec<TextChunk> {
        self.gather_with_inherited_style(parent_style)
    }
}

#[derive(Debug, Clone)]
pub enum TextNodeAddItem {
    Text(String),
    Node(TextNodeRenderable),
    StyledText(StyledText),
}

#[derive(Debug, Clone)]
pub struct InheritedStyle {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<UrlLink>,
}

impl InheritedStyle {
    #[must_use]
    pub fn new() -> Self {
        InheritedStyle {
            fg: None,
            bg: None,
            attributes: 0,
            link: None,
        }
    }
}

impl Default for InheritedStyle {
    fn default() -> Self {
        Self::new()
    }
}

fn styled_text_to_text_nodes(st: &StyledText) -> Vec<TextNodeRenderable> {
    st.chunks
        .iter()
        .map(|chunk| {
            let mut node = TextNodeRenderable::new(TextNodeOptions {
                fg: chunk.fg.map(ColorInput::RGBA),
                bg: chunk.bg.map(ColorInput::RGBA),
                attributes: Some(chunk.attributes),
                link: None,
                id: None,
            });
            node.add(TextNodeAddItem::Text(chunk.text.clone()));
            node
        })
        .collect()
}
