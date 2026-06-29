#![allow(
    clippy::box_default,
    clippy::doc_markdown,
    clippy::used_underscore_items,
    clippy::default_constructed_unit_structs
)]

//! Unit tests for implemented solid top-level modules:
//!   - reconciler
//!   - scrollback
//!   - time_to_first_draw

mod reconciler_tests {
    use crate::core::renderable::{Renderable, RootRenderable};
    use crate::solid::reconciler::{
        DomNode, TextNode,
        insert_node, remove_node,
        create_text_node, create_element, create_slot_node,
        replace_text, is_text_node,
        get_parent_node, get_first_child, get_next_sibling,
        set_property,
    };

    #[test]
    fn test_dom_node_type() {
        let _node: DomNode = Box::new(RootRenderable::new());
    }

    #[test]
    fn test_text_node_from_string() {
        let node = TextNode::from_string("hello", None);
        assert!(!is_text_node(&node));
    }

    #[test]
    fn test_insert_node_no_anchor() {
        let mut root = RootRenderable::new();
        let child = Box::new(RootRenderable::new());
        insert_node(&mut root, child, None);
        assert_eq!(root.children_count(), 1);
    }

    #[test]
    fn test_insert_node_with_anchor() {
        let mut root = RootRenderable::new();
        let first = Box::new(RootRenderable::new());
        insert_node(&mut root, first, None);
        let third = Box::new(RootRenderable::new());
        insert_node(&mut root, third, Some("__root__"));
        assert_eq!(root.children_count(), 2);
    }

    #[test]
    fn test_remove_node() {
        let mut root = RootRenderable::new();
        root.add_child(Box::new(RootRenderable::new()));
        remove_node(&mut root, "__root__");
        assert_eq!(root.children_count(), 0);
    }

    #[test]
    fn test_remove_node_nonexistent() {
        let mut root = RootRenderable::new();
        remove_node(&mut root, "nonexistent");
    }

    #[test]
    fn test_create_text_node() {
        let node = create_text_node("hello");
        let _ = node;
    }

    #[test]
    fn test_create_element() {
        let node = create_element("box");
        let _ = node;
    }

    #[test]
    fn test_create_slot_node() {
        let node = create_slot_node();
        assert!(!is_text_node(&node));
    }

    #[test]
    fn test_replace_text_noop() {
        let mut node = create_text_node("old");
        replace_text(&mut node, "new");
    }

    #[test]
    fn test_is_text_node_always_false() {
        let node = create_text_node("test");
        assert!(!is_text_node(&node));
        let element = create_element("box");
        assert!(!is_text_node(&element));
    }

    #[test]
    fn test_get_parent_node_always_none() {
        let node = create_text_node("orphan");
        assert!(get_parent_node(&node).is_none());
    }

    #[test]
    fn test_get_first_child_empty() {
        let node = create_element("parent");
        assert!(get_first_child(&node).is_none());
    }

    #[test]
    fn test_get_next_sibling_always_none() {
        let node = create_text_node("only");
        assert!(get_next_sibling(&node).is_none());
    }

    #[test]
    fn test_set_property_id() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "id", "my-custom-id");
        assert_eq!(root.id(), "my-custom-id");
    }

    #[test]
    fn test_set_property_visible_true() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "visible", "true");
        assert!(root.is_visible());
    }

    #[test]
    fn test_set_property_visible_false() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "visible", "false");
        assert!(!root.is_visible());
    }

    #[test]
    fn test_set_property_visible_invalid_ignored() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "visible", "notabool");
        assert!(root.is_visible());
    }

    #[test]
    fn test_set_property_focusable() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "focusable", "true");
        assert!(root.is_focusable());
        set_property(&mut root, "focusable", "false");
        assert!(!root.is_focusable());
    }

    #[test]
    fn test_set_property_unknown_ignored() {
        let mut root = RootRenderable::new();
        set_property(&mut root, "nonexistent", "value");
    }
}

mod scrollback_tests {
    use crate::solid::scrollback::{
        SolidScrollbackWriterOptions, ScrollbackRenderContext,
        _create_scrollback_writer,
    };

    #[test]
    fn test_scrollback_writer_options_default() {
        let opts = SolidScrollbackWriterOptions::default();
        assert_eq!(opts.width, None);
        assert_eq!(opts.height, None);
        assert_eq!(opts.row_columns, None);
        assert_eq!(opts.start_on_new_line, Some(true));
        assert_eq!(opts.trailing_newline, None);
    }

    #[test]
    fn test_scrollback_writer_options_custom() {
        let opts = SolidScrollbackWriterOptions {
            width: Some(80),
            height: Some(24),
            row_columns: Some(80),
            start_on_new_line: Some(false),
            trailing_newline: Some(true),
        };
        assert_eq!(opts.width, Some(80));
        assert_eq!(opts.height, Some(24));
    }

    #[test]
    fn test_scrollback_render_context() {
        let ctx = ScrollbackRenderContext { width: 80, tail_column: 0 };
        assert_eq!(ctx.width, 80);
        assert_eq!(ctx.tail_column, 0);
    }

    #[test]
    #[should_panic(expected = "requires BoxRenderable")]
    fn test_create_scrollback_writer_panics_on_call() {
        let writer = _create_scrollback_writer(
            Box::new(|_ctx: &ScrollbackRenderContext| {}),
            SolidScrollbackWriterOptions::default(),
        );
        // The panic happens when the writer is called, not when created
        writer(ScrollbackRenderContext { width: 80, tail_column: 0 });
    }
}

mod time_to_first_draw_tests {
    use crate::solid::time_to_first_draw::TimeToFirstDrawRenderable;

    #[test]
    fn test_new() {
        let _ttfd = TimeToFirstDrawRenderable::new();
    }

    #[test]
    fn test_default() {
        let _ttfd = TimeToFirstDrawRenderable::default();
    }
}
