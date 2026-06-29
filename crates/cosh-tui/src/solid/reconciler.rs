use crate::core::renderable::{Renderable, RootRenderable};

pub type DomNode = Box<dyn Renderable>;

pub struct TextNode;

impl TextNode {
    #[must_use]
    pub fn from_string(_text: &str, _options: Option<&str>) -> DomNode {
        Box::new(RootRenderable::new())
    }
}

pub fn insert_node(parent: &mut RootRenderable, node: DomNode, anchor: Option<&str>) {
    if let Some(anchor_id) = anchor {
        parent.insert_child_before(node, anchor_id);
    } else {
        parent.add_child(node);
    }
}

pub fn remove_node(parent: &mut RootRenderable, node_id: &str) {
    parent.remove_child(node_id);
}

#[must_use]
pub fn create_text_node(value: &str) -> DomNode {
    TextNode::from_string(value, None)
}

#[must_use]
pub fn create_element(_tag_name: &str) -> DomNode {
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

#[allow(clippy::needless_pass_by_value)]
pub fn set_property(node: &mut RootRenderable, name: &str, value: &str) {
    match name {
        "id" => node.set_id(value.to_string()),
        "visible" => {
            if let Ok(v) = value.parse::<bool>() {
                node.set_visible(v);
            }
        }
        "focusable" => {
            if let Ok(v) = value.parse::<bool>() {
                node.set_focusable(v);
            }
        }
        _ => {}
    }
}
