use crate::core::renderable::{Renderable, RootRenderable};

#[test]
fn test_root_renderable_new() {
    let root = RootRenderable::new();
    assert_eq!(root.id(), "__root__");
    assert!(root.num() > 0);
    assert!(root.is_visible());
    assert!(!root.is_focusable());
    assert!(!root.is_destroyed());
    assert!(root.parent_num().is_none());
}

#[test]
fn test_root_renderable_default() {
    let root = RootRenderable::default();
    assert_eq!(root.id(), "__root__");
}

#[test]
fn test_root_renderable_set_id() {
    let mut root = RootRenderable::new();
    root.set_id("custom-id".into());
    assert_eq!(root.id(), "custom-id");
}

#[test]
fn test_root_renderable_set_visible() {
    let mut root = RootRenderable::new();
    root.set_visible(false);
    assert!(!root.is_visible());
}

#[test]
fn test_root_renderable_set_focusable() {
    let mut root = RootRenderable::new();
    root.set_focusable(true);
    assert!(root.is_focusable());
}

#[test]
fn test_root_renderable_add_child() {
    let mut root = RootRenderable::new();
    let idx = root.add_child(Box::new(RootRenderable::new()));
    assert_eq!(idx, 0);
    assert_eq!(root.children().len(), 1);
}

#[test]
fn test_root_renderable_add_children() {
    let mut root = RootRenderable::new();
    root.add_child(Box::new(RootRenderable::new()));
    root.add_child(Box::new(RootRenderable::new()));
    assert_eq!(root.children().len(), 2);
}

#[test]
fn test_root_renderable_remove_child() {
    let mut root = RootRenderable::new();
    root.add_child(Box::new(RootRenderable::new()));
    root.remove_child("__root__");
    assert_eq!(root.children().len(), 0);
}

#[test]
fn test_root_renderable_remove_nonexistent() {
    let mut root = RootRenderable::new();
    root.remove_child("nonexistent");
}

#[test]
fn test_root_renderable_insert_child_before() {
    let mut root = RootRenderable::new();
    root.add_child(Box::new(RootRenderable::new()));
    let result = root.insert_child_before(Box::new(RootRenderable::new()), "__root__");
    assert!(result.is_some());
    assert_eq!(root.children().len(), 2);
}

#[test]
fn test_root_renderable_insert_before_nonexistent() {
    let mut root = RootRenderable::new();
    let result = root.insert_child_before(Box::new(RootRenderable::new()), "ghost");
    assert!(result.is_none());
}

#[test]
fn test_root_renderable_children_count() {
    let mut root = RootRenderable::new();
    assert_eq!(root.children_count(), 0);
    root.add_child(Box::new(RootRenderable::new()));
    assert_eq!(root.children_count(), 1);
}

#[test]
fn test_root_renderable_render_self_noop() {
    let root = RootRenderable::new();
    let area = ratatui::layout::Rect::new(0, 0, 10, 10);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    root.render_self(&mut buf, area);
}

#[test]
fn test_renderable_trait_object() {
    let root = RootRenderable::new();
    let trait_obj: &dyn Renderable = &root;
    assert_eq!(trait_obj.id(), "__root__");
    assert!(trait_obj.is_visible());
}
