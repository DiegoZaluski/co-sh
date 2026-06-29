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
        DomNode, TextNode, create_element, create_slot_node, create_text_node, get_first_child,
        get_next_sibling, get_parent_node, insert_node, is_text_node, remove_node, replace_text,
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
        let mut root: DomNode = Box::new(RootRenderable::new());
        let mut child: DomNode = Box::new(RootRenderable::new());
        insert_node(&mut root, &mut child, None);
        assert_eq!(root.children_count(), 1);
    }

    #[test]
    fn test_insert_node_with_anchor() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        let mut first: DomNode = Box::new(RootRenderable::new());
        insert_node(&mut root, &mut first, None);
        let mut third: DomNode = Box::new(RootRenderable::new());
        insert_node(&mut root, &mut third, Some("__root__"));
        assert_eq!(root.children_count(), 2);
    }

    #[test]
    fn test_remove_node() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        root.add_child(Box::new(RootRenderable::new()));
        remove_node(&mut root, "__root__");
        assert_eq!(root.children_count(), 0);
    }

    #[test]
    fn test_remove_node_nonexistent() {
        let mut root: DomNode = Box::new(RootRenderable::new());
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
    fn test_text_node_from_string_preserves_text() {
        let node = TextNode::from_string("hello world", None);
        // Should create a text-containing node, not just an empty root
        assert!(
            node.id().starts_with("text-"),
            "TextNode should have text- prefix, got: {}",
            node.id()
        );
    }

    #[test]
    fn test_set_property_id() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "id", "my-custom-id");
        assert_eq!(root.id(), "my-custom-id");
    }

    #[test]
    fn test_set_property_visible_true() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "visible", "true");
        assert!(root.is_visible());
    }

    #[test]
    fn test_set_property_visible_false() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "visible", "false");
        assert!(!root.is_visible());
    }

    #[test]
    fn test_set_property_visible_invalid_ignored() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "visible", "notabool");
        assert!(root.is_visible());
    }

    #[test]
    fn test_set_property_focusable() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "focusable", "true");
        assert!(root.is_focusable());
        set_property(&mut root, "focusable", "false");
        assert!(!root.is_focusable());
    }

    #[test]
    fn test_set_property_unknown_ignored() {
        let mut root: DomNode = Box::new(RootRenderable::new());
        set_property(&mut root, "nonexistent", "value");
    }

    #[test]
    fn test_set_property_box_background() {
        let mut box_node: DomNode = Box::new(crate::core::renderables::r#box::BoxRenderable::new());
        set_property(&mut box_node, "background", "#ff0000");
        if let Some(b) = box_node
            .as_any_mut()
            .downcast_mut::<crate::core::renderables::r#box::BoxRenderable>()
        {
            assert_eq!(b.background_color().r(), 1.0);
            assert_eq!(b.background_color().g(), 0.0);
            assert_eq!(b.background_color().b(), 0.0);
        } else {
            panic!("expected BoxRenderable");
        }
    }

    #[test]
    fn test_set_property_box_border() {
        let mut box_node: DomNode = Box::new(crate::core::renderables::r#box::BoxRenderable::new());
        set_property(&mut box_node, "border", "true");
        if let Some(b) = box_node
            .as_any_mut()
            .downcast_mut::<crate::core::renderables::r#box::BoxRenderable>()
        {
            assert!(b.border_sides().top);
        } else {
            panic!("expected BoxRenderable");
        }
    }

    #[test]
    fn test_set_property_box_title() {
        let mut box_node: DomNode = Box::new(crate::core::renderables::r#box::BoxRenderable::new());
        set_property(&mut box_node, "title", "Hello World");
        if let Some(b) = box_node
            .as_any_mut()
            .downcast_mut::<crate::core::renderables::r#box::BoxRenderable>()
        {
            assert_eq!(b.title(), Some("Hello World"));
        } else {
            panic!("expected BoxRenderable");
        }
    }

    #[test]
    fn test_create_element_span_from_catalogue() {
        let node = create_element("span");
        assert!(node.id().starts_with("span-"));
    }

    #[test]
    fn test_create_element_br_from_catalogue() {
        let node = create_element("br");
        assert!(node.id().starts_with("br-"));
    }

    #[test]
    fn test_create_element_unknown_falls_back_to_root() {
        let node = create_element("nonexistent");
        assert_eq!(node.id(), "__root__");
    }
}

mod scrollback_tests {
    use crate::solid::scrollback::{
        ScrollbackRenderContext, SolidScrollbackWriterOptions, create_scrollback_writer,
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
        let ctx = ScrollbackRenderContext {
            width: 80,
            tail_column: 0,
        };
        assert_eq!(ctx.width, 80);
        assert_eq!(ctx.tail_column, 0);
    }

    #[test]
    fn testcreate_scrollback_writer_returns_snapshot() {
        let writer = create_scrollback_writer(
            Box::new(|_ctx: &ScrollbackRenderContext| {}),
            SolidScrollbackWriterOptions::default(),
        );
        let snapshot = writer(ScrollbackRenderContext {
            width: 80,
            tail_column: 0,
        });
        assert_eq!(snapshot.width, 80);
        assert!(snapshot.start_on_new_line);
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

mod plugins_slot_tests {
    use crate::core::renderable::Renderable;
    use crate::solid::plugins::slot::{PluginErrorEvent, SlotMode, SlotRegistry};

    fn dummy_renderable() -> Box<dyn Renderable> {
        Box::new(crate::core::renderable::RootRenderable::new())
    }

    #[test]
    fn test_slot_registry_new() {
        let registry = SlotRegistry::new();
        assert_eq!(registry.slot_count(), 0);
    }

    #[test]
    fn test_slot_registry_register_and_resolve() {
        let mut registry = SlotRegistry::new();
        registry.register("header", "plugin-a", Box::new(dummy_renderable));
        assert!(registry.has_entries("header"));
        let entries = registry.resolve("header", SlotMode::Append);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "plugin-a");
    }

    #[test]
    fn test_slot_registry_resolve_single_winner() {
        let mut registry = SlotRegistry::new();
        registry.register("sidebar", "p1", Box::new(dummy_renderable));
        registry.register("sidebar", "p2", Box::new(dummy_renderable));
        let entries = registry.resolve("sidebar", SlotMode::SingleWinner);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "p1");
    }

    #[test]
    fn test_slot_registry_resolve_replace() {
        let mut registry = SlotRegistry::new();
        registry.register("content", "p1", Box::new(dummy_renderable));
        registry.register("content", "p2", Box::new(dummy_renderable));
        let entries = registry.resolve("content", SlotMode::Replace);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_slot_registry_resolve_unknown_slot() {
        let registry = SlotRegistry::new();
        let entries = registry.resolve("nonexistent", SlotMode::Append);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_slot_registry_unregister() {
        let mut registry = SlotRegistry::new();
        registry.register("footer", "p1", Box::new(dummy_renderable));
        assert!(registry.has_entries("footer"));
        registry.unregister("footer");
        assert!(!registry.has_entries("footer"));
    }

    #[test]
    fn test_slot_registry_has_entries_empty_slot() {
        let registry = SlotRegistry::new();
        assert!(!registry.has_entries("empty"));
    }

    #[test]
    fn test_slot_registry_error_handling() {
        use std::sync::{Arc, Mutex};
        let mut registry = SlotRegistry::new();
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let errors_clone = Arc::clone(&errors);
        registry.on_error(Box::new(move |event: &PluginErrorEvent| {
            errors_clone.lock().unwrap().push(event.plugin_id.clone());
        }));
        registry.report_error(&PluginErrorEvent {
            plugin_id: "test-plugin".to_string(),
            slot_name: "test-slot".to_string(),
            phase: "render".to_string(),
            source: "test".to_string(),
            error: "something broke".to_string(),
        });
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(errors.lock().unwrap()[0], "test-plugin");
    }

    #[test]
    fn test_slot_mode_default() {
        let mode: SlotMode = Default::default();
        assert_eq!(mode, SlotMode::Append);
    }

    #[test]
    fn test_slot_registry_default() {
        let registry = SlotRegistry::default();
        assert_eq!(registry.slot_count(), 0);
    }

    #[test]
    fn test_slot_registry_multiple_entries_same_slot() {
        let mut registry = SlotRegistry::new();
        registry.register("nav", "p1", Box::new(dummy_renderable));
        registry.register("nav", "p2", Box::new(dummy_renderable));
        registry.register("nav", "p3", Box::new(dummy_renderable));
        let entries = registry.resolve("nav", SlotMode::Append);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, "p1");
        assert_eq!(entries[1].id, "p2");
        assert_eq!(entries[2].id, "p3");
    }
}
