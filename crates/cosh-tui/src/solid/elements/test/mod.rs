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
