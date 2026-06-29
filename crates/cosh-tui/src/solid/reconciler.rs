use crate::core::renderable::{Renderable, RootRenderable};
use crate::core::renderables::r#box::BoxRenderable;
use crate::core::renderables::text::{TextRenderable, string_to_styled_text};
use crate::solid::elements::catalogue::create_component;

pub type DomNode = Box<dyn Renderable>;

pub struct TextNode;

impl TextNode {
    #[must_use]
    pub fn from_string(text: &str, _options: Option<&str>) -> DomNode {
        let styled = string_to_styled_text(text);
        Box::new(TextRenderable::new(Some(styled)))
    }
}

pub fn insert_node(parent: &mut DomNode, node: &mut DomNode, anchor: Option<&str>) {
    let p: &mut dyn Renderable = parent.as_mut();
    if let Some(anchor_id) = anchor {
        p.insert_child_before(
            std::mem::replace(node, Box::new(RootRenderable::new())),
            anchor_id,
        );
    } else {
        p.add_child(std::mem::replace(node, Box::new(RootRenderable::new())));
    }
}

pub fn remove_node(parent: &mut DomNode, node_id: &str) {
    let p: &mut dyn Renderable = parent.as_mut();
    p.remove_child(node_id);
}

#[must_use]
pub fn create_text_node(value: &str) -> DomNode {
    TextNode::from_string(value, None)
}

#[must_use]
pub fn create_element(tag_name: &str) -> DomNode {
    if let Some(node) = create_component(tag_name) {
        return node;
    }
    Box::new(RootRenderable::new())
}

#[must_use]
pub fn create_slot_node() -> DomNode {
    Box::new(RootRenderable::new())
}

pub fn replace_text(_text_node: &mut DomNode, _value: &str) {}

#[must_use]
pub fn is_text_node(_node: &DomNode) -> bool {
    false
}

#[must_use]
pub fn get_parent_node(_node: &DomNode) -> Option<&DomNode> {
    None
}

pub fn get_first_child(node: &DomNode) -> Option<&dyn Renderable> {
    node.children().first().map(std::convert::AsRef::as_ref)
}

#[must_use]
pub fn get_next_sibling(_node: &DomNode) -> Option<&DomNode> {
    None
}

pub fn set_property(node: &mut DomNode, name: &str, value: &str) {
    let n: &mut dyn Renderable = node.as_mut();
    match name {
        "id" => {
            if let Some(r) = n.as_any_mut().downcast_mut::<RootRenderable>() {
                r.set_id(value.to_string());
            }
        }
        "visible" => {
            if let Ok(v) = value.parse::<bool>()
                && let Some(r) = n.as_any_mut().downcast_mut::<RootRenderable>()
            {
                r.set_visible(v);
            }
        }
        "focusable" => {
            if let Ok(v) = value.parse::<bool>()
                && let Some(r) = n.as_any_mut().downcast_mut::<RootRenderable>()
            {
                r.set_focusable(v);
            }
        }
        "background" => {
            if let Some(b) = n.as_any_mut().downcast_mut::<BoxRenderable>() {
                b.set_background_color(Some(value.into()));
            }
        }
        "border" => {
            if let Some(b) = n.as_any_mut().downcast_mut::<BoxRenderable>()
                && let Ok(v) = value.parse::<bool>()
            {
                b.set_border(v);
            }
        }
        "title" => {
            if let Some(b) = n.as_any_mut().downcast_mut::<BoxRenderable>() {
                b.set_title(Some(value.to_string()));
            }
        }
        _ => {}
    }
}
