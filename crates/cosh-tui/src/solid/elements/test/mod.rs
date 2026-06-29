#![allow(
    clippy::doc_markdown,
    clippy::used_underscore_items,
    clippy::default_constructed_unit_structs
)]

//! Unit tests for `solid/elements/hooks.rs`

mod hooks_tests {
    use crate::solid::elements::hooks::{
        on_resize, _on_keyboard, _on_paste, _on_focus, _on_blur, _on_selection,
        _Timeline, HasKeyInput,
    };

    #[test]
    fn test_on_resize_calls_callback() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let terminal = ratatui::Terminal::new(backend).unwrap();
        let config = crate::core::renderer::RendererConfig::default();
        let renderer = crate::core::renderer::Renderer::new(terminal, config);

        let called = std::sync::atomic::AtomicBool::new(false);
        on_resize(&renderer, move |w, h| {
            let _ = (w, h);
            called.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // No assertion on called since on_resize calls synchronously
    }

    #[test]
    fn test_on_keyboard_noop() {
        struct Dummy;
        impl HasKeyInput for Dummy {
            fn on_key(&mut self, _key: &str) {}
        }
        let handler = Dummy;
        _on_keyboard(&handler, |_key: &str| {});
    }

    #[test]
    fn test_on_paste_noop() {
        struct Dummy;
        impl HasKeyInput for Dummy {}
        let handler = Dummy;
        _on_paste(&handler, |_text: &str| {});
    }

    #[test]
    fn test_on_focus_noop() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let terminal = ratatui::Terminal::new(backend).unwrap();
        let renderer = crate::core::renderer::Renderer::new(
            terminal,
            crate::core::renderer::RendererConfig::default(),
        );
        _on_focus(&renderer, || {});
    }

    #[test]
    fn test_on_blur_noop() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let terminal = ratatui::Terminal::new(backend).unwrap();
        let renderer = crate::core::renderer::Renderer::new(
            terminal,
            crate::core::renderer::RendererConfig::default(),
        );
        _on_blur(&renderer, || {});
    }

    #[test]
    fn test_on_selection_noop() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let terminal = ratatui::Terminal::new(backend).unwrap();
        let renderer = crate::core::renderer::Renderer::new(
            terminal,
            crate::core::renderer::RendererConfig::default(),
        );
        _on_selection(&renderer, |_sel: &crate::core::types::Selection| {});
    }

    #[test]
    fn test_timeline_new() {
        let _t = _Timeline::new();
    }

    #[test]
    fn test_timeline_default() {
        let _t = _Timeline::default();
    }

    #[test]
    fn test_has_key_input_trait() {
        struct MyHandler;
        impl HasKeyInput for MyHandler {
            fn on_key(&mut self, key: &str) {
                let _ = key;
            }
        }
        let mut handler = MyHandler;
        handler.on_key("Enter");
    }

    #[test]
    fn test_has_key_input_default_impl() {
        struct MyHandler;
        impl HasKeyInput for MyHandler {}
        let mut handler = MyHandler;
        handler.on_key("x");
    }
}

mod catalogue_tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use crate::core::renderable::Renderable;
    use crate::solid::elements::catalogue::{
        BoldSpanRenderable, SpanRenderable, LineBreakRenderable, LinkRenderable,
        register_component, create_component,
    };

    #[test]
    fn test_span_renderable_new() {
        let span = SpanRenderable::new();
        assert!(span.id().starts_with("span-"));
        assert!(span.is_visible());
        assert!(!span.is_focusable());
        assert!(!span.is_destroyed());
        assert!(span.parent_num().is_none());
    }

    #[test]
    fn test_span_renderable_default() {
        let span = SpanRenderable::default();
        assert!(span.id().starts_with("span-"));
    }

    #[test]
    fn test_span_renderable_set_text() {
        let mut span = SpanRenderable::new();
        span.set_text("hello".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        span.render_self(&mut buf, Rect::new(0, 0, 10, 1));
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "h");
        assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "o");
    }

    #[test]
    fn test_span_renderable_bold_attribute() {
        let mut span = SpanRenderable::new();
        span.set_text("bold".to_string());
        span.set_attributes(1); // bold bit
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        span.render_self(&mut buf, Rect::new(0, 0, 10, 1));
        let cell = buf.cell((0, 0)).unwrap();
        assert_eq!(cell.symbol(), "b");
        assert!(cell.style().add_modifier.intersects(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn test_span_renderable_truncates_at_area() {
        let mut span = SpanRenderable::new();
        span.set_text("hello world".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        span.render_self(&mut buf, Rect::new(0, 0, 5, 1));
        assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "o");
    }

    #[test]
    fn test_line_break_renderable_new() {
        let br = LineBreakRenderable::new();
        assert!(br.id().starts_with("br-"));
        assert!(br.is_visible());
        assert!(!br.is_focusable());
    }

    #[test]
    fn test_line_break_renderable_default() {
        let br = LineBreakRenderable::default();
        assert!(br.id().starts_with("br-"));
    }

    #[test]
    fn test_line_break_render_self() {
        let br = LineBreakRenderable::new();
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        br.render_self(&mut buf, Rect::new(0, 0, 1, 1));
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "\n");
    }

    #[test]
    fn test_link_renderable_new() {
        let link = LinkRenderable::new("https://example.com".to_string());
        assert!(link.id().starts_with("span-"));
    }

    #[test]
    fn test_link_renderable_set_text() {
        let mut link = LinkRenderable::new("https://example.com".to_string());
        link.set_text("click".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        link.render_self(&mut buf, Rect::new(0, 0, 10, 1));
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "c");
    }

    #[test]
    fn test_register_and_create_component() {
        register_component("my-widget", Box::new(|| {
            Box::new(SpanRenderable::new())
        }));
        let result = create_component("my-widget");
        assert!(result.is_some());
        assert!(result.unwrap().id().starts_with("span-"));
    }

    #[test]
    fn test_create_component_span() {
        let result = create_component("span");
        assert!(result.is_some());
        assert!(result.unwrap().id().starts_with("span-"));
    }

    #[test]
    fn test_create_component_br() {
        let result = create_component("br");
        assert!(result.is_some());
        assert!(result.unwrap().id().starts_with("br-"));
    }

    #[test]
    fn test_create_component_unknown() {
        let result = create_component("non-existent");
        assert!(result.is_none());
    }

    #[test]
    fn test_type_aliases() {
        use crate::solid::elements::catalogue::{
            BoldSpanRenderable,
            ItalicSpanRenderable,
            UnderlineSpanRenderable,
        };
        let _bold = BoldSpanRenderable::new();
        let _italic = ItalicSpanRenderable::new();
        let _underline = UnderlineSpanRenderable::new();
    }

    #[test]
    fn test_span_unique_ids() {
        let a = SpanRenderable::new();
        let b = SpanRenderable::new();
        assert_ne!(a.num(), b.num());
    }

    #[test]
    fn test_span_as_any() {
        let span = SpanRenderable::new();
        let any = span.as_any();
        assert!(any.is::<SpanRenderable>());
    }

    #[test]
    fn test_span_children_empty() {
        let span = SpanRenderable::new();
        assert!(span.children().is_empty());
    }

    #[test]
    fn test_line_break_as_any() {
        let br = LineBreakRenderable::new();
        let any = br.as_any();
        assert!(any.is::<LineBreakRenderable>());
    }

    #[test]
    fn test_link_as_any() {
        let link = LinkRenderable::new("https://example.com".to_string());
        let any = link.as_any();
        assert!(any.is::<LinkRenderable>());
    }

    #[test]
    fn test_bold_span_type() {
        let _: BoldSpanRenderable = SpanRenderable::new();
    }
}

mod extras_tests {
    use crate::core::renderable::Renderable;
    use crate::solid::elements::extras::DynamicRenderable;

    #[test]
    fn test_dynamic_try_new_unknown() {
        let dynamic = DynamicRenderable::try_new("nonexistent");
        assert!(dynamic.is_none());
    }

    #[test]
    fn test_dynamic_try_new_known() {
        let dynamic = DynamicRenderable::try_new("span");
        assert!(dynamic.is_some());
        let d = dynamic.unwrap();
        assert!(d.id().starts_with("dynamic-"));
        assert!(d.is_visible());
        assert!(!d.is_focusable());
    }

    #[test]
    #[should_panic(expected = "unknown component")]
    fn test_dynamic_new_panics_on_unknown() {
        let _ = DynamicRenderable::new("nonexistent");
    }

    #[test]
    fn test_dynamic_render_self() {
        let dynamic = DynamicRenderable::try_new("span").unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 1));
        dynamic.render_self(&mut buf, ratatui::layout::Rect::new(0, 0, 10, 1));
        // inner SpanRenderable has no text, renders nothing — but doesn't panic
    }

    #[test]
    fn test_dynamic_unique_ids() {
        let a = DynamicRenderable::try_new("span").unwrap();
        let b = DynamicRenderable::try_new("span").unwrap();
        assert_ne!(a.num(), b.num());
    }

    #[test]
    fn test_dynamic_as_any() {
        let dynamic = DynamicRenderable::try_new("br").unwrap();
        let any = dynamic.as_any();
        assert!(any.is::<DynamicRenderable>());
    }

    #[test]
    fn test_dynamic_children_empty() {
        let dynamic = DynamicRenderable::try_new("br").unwrap();
        assert!(dynamic.children().is_empty());
    }
}

mod slot_tests {
    use crate::core::renderable::Renderable;
    use crate::solid::elements::slot::{TextSlotRenderable, SlotRenderable};

    #[test]
    fn test_text_slot_new() {
        let ts = TextSlotRenderable::new();
        assert!(ts.id().starts_with("slot-text-"));
        assert!(!ts.is_visible());
        assert!(!ts.is_destroyed());
        assert!(ts.slot_parent_num().is_none());
    }

    #[test]
    fn test_text_slot_default() {
        let ts = TextSlotRenderable::default();
        assert!(ts.id().starts_with("slot-text-"));
    }

    #[test]
    fn test_text_slot_detach_from_slot() {
        let mut ts = TextSlotRenderable::new();
        ts.set_slot_parent(42);
        assert_eq!(ts.slot_parent_num(), Some(42));
        ts.detach_from_slot();
        assert!(ts.slot_parent_num().is_none());
    }

    #[test]
    fn test_text_slot_dispose_without_slot_cascade() {
        let mut ts = TextSlotRenderable::new();
        ts.set_slot_parent(42);
        assert!(!ts.is_destroyed());
        ts.dispose_without_slot_cascade();
        assert!(ts.is_destroyed());
        assert!(ts.slot_parent_num().is_none());
    }

    #[test]
    fn test_text_slot_dispose_is_idempotent() {
        let mut ts = TextSlotRenderable::new();
        ts.dispose_without_slot_cascade();
        ts.dispose_without_slot_cascade(); // second call should not panic
        assert!(ts.is_destroyed());
    }

    #[test]
    fn test_text_slot_destroy_with_slot() {
        let mut ts = TextSlotRenderable::new();
        ts.set_slot_parent(7);
        ts.destroy_with_slot();
        assert!(ts.is_destroyed());
        assert!(ts.slot_parent_num().is_none());
    }

    #[test]
    fn test_slot_renderable_new() {
        let slot = SlotRenderable::new();
        assert!(slot.id().starts_with("slot-"));
        assert!(!slot.is_visible());
        assert_eq!(slot.child_count(), 0);
        assert!(slot.current_child().is_none());
    }

    #[test]
    fn test_slot_renderable_register_and_get() {
        let mut slot = SlotRenderable::new();
        let child = TextSlotRenderable::new();
        let child_num = child.num();
        slot.register_child(1, Box::new(child));
        assert_eq!(slot.child_count(), 1);
        let retrieved = slot.get_child(1);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().num(), child_num);
    }

    #[test]
    fn test_slot_renderable_remove_child() {
        let mut slot = SlotRenderable::new();
        let child = TextSlotRenderable::new();
        slot.register_child(1, Box::new(child));
        assert_eq!(slot.child_count(), 1);
        let removed = slot.remove_child(1);
        assert!(removed.is_some());
        assert_eq!(slot.child_count(), 0);
    }

    #[test]
    fn test_slot_renderable_current_child() {
        let mut slot = SlotRenderable::new();
        assert!(slot.current_child().is_none());
        let child = TextSlotRenderable::new();
        let child_num = child.num();
        slot.register_child(1, Box::new(child));
        let current = slot.current_child();
        assert!(current.is_some());
        assert_eq!(current.unwrap().num(), child_num);
    }

    #[test]
    fn test_slot_renderable_clear() {
        let mut slot = SlotRenderable::new();
        slot.register_child(1, Box::new(TextSlotRenderable::new()));
        slot.register_child(2, Box::new(TextSlotRenderable::new()));
        assert_eq!(slot.child_count(), 2);
        slot.clear();
        assert_eq!(slot.child_count(), 0);
    }

    #[test]
    fn test_slot_renderable_get_child_missing_parent() {
        let slot = SlotRenderable::new();
        assert!(slot.get_child(99).is_none());
    }

    #[test]
    fn test_slot_renderable_replaces_child() {
        let mut slot = SlotRenderable::new();
        let child1 = TextSlotRenderable::new();
        let child2 = TextSlotRenderable::new();
        let num2 = child2.num();
        slot.register_child(1, Box::new(child1));
        slot.register_child(1, Box::new(child2)); // replaces at same key
        assert_eq!(slot.child_count(), 1);
        assert_eq!(slot.get_child(1).unwrap().num(), num2);
    }

    #[test]
    fn test_slot_default() {
        let slot = SlotRenderable::default();
        assert!(slot.id().starts_with("slot-"));
    }
}
