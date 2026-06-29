#![allow(
    clippy::doc_markdown,
    clippy::used_underscore_items,
    clippy::used_underscore_binding,
    clippy::items_after_statements
)]

//! Unit tests for `solid/renderer/`

mod universal_tests {
    use crate::solid::renderer::universal::{RendererOptions, Renderer};

    struct TestNode {
        tag: String,
    }

    struct TestRendererOptions;

    impl RendererOptions for TestRendererOptions {
        type Node = TestNode;

        fn create_element(&mut self, tag: &str) -> TestNode {
            TestNode { tag: tag.to_string() }
        }

        fn create_text_node(&mut self, value: &str) -> TestNode {
            TestNode { tag: format!("text:{value}") }
        }

        fn create_slot_node(&mut self) -> TestNode {
            TestNode { tag: "slot".to_string() }
        }

        fn replace_text(&mut self, _text_node: &TestNode, _value: &str) {}

        fn is_text_node(&self, node: &TestNode) -> bool {
            node.tag.starts_with("text:")
        }

        fn set_property<T>(&mut self, _node: &TestNode, _name: &str, _value: T, _prev: Option<T>) {}

        fn insert_node(&mut self, _parent: &TestNode, _node: TestNode, _anchor: Option<&TestNode>) {}

        fn remove_node(&mut self, _parent: &TestNode, _node: &TestNode) {}

        fn get_parent_node(&self, _node: &TestNode) -> Option<&TestNode> { None }
        fn get_first_child(&self, _node: &TestNode) -> Option<&TestNode> { None }
        fn get_next_sibling(&self, _node: &TestNode) -> Option<&TestNode> { None }
    }

    #[test]
    fn test_renderer_options_create_element() {
        let mut opts = TestRendererOptions;
        let element = opts.create_element("div");
        assert_eq!(element.tag, "div");
    }

    #[test]
    fn test_renderer_options_text_detection() {
        let mut opts = TestRendererOptions;
        let text = opts.create_text_node("hello");
        assert!(opts.is_text_node(&text));
        let element = opts.create_element("span");
        assert!(!opts.is_text_node(&element));
    }

    #[test]
    fn test_renderer_options_queries_return_none() {
        let mut opts = TestRendererOptions;
        let node = opts.create_element("orphan");
        assert!(opts.get_parent_node(&node).is_none());
        assert!(opts.get_first_child(&node).is_none());
        assert!(opts.get_next_sibling(&node).is_none());
    }

    struct TestRenderer;

    impl Renderer<TestNode> for TestRenderer {
        fn render(&mut self, _code: fn() -> TestNode, _node: TestNode) -> Box<dyn FnOnce()> {
            Box::new(|| {})
        }

        fn effect<T>(&mut self, _f: Box<dyn FnMut(Option<T>) -> T>, _init: Option<T>) {}

        fn memo<T: 'static>(&mut self, f: Box<dyn Fn() -> T>, _equal: bool) -> Box<dyn Fn() -> T> {
            Box::new(f)
        }

        fn create_component<T>(_comp: fn(T) -> TestNode, _props: T) -> TestNode {
            TestNode { tag: "component".to_string() }
        }

        fn create_element(&mut self, tag: &str) -> TestNode {
            TestNode { tag: tag.to_string() }
        }

        fn create_text_node(&mut self, value: &str) -> TestNode {
            TestNode { tag: format!("text:{value}") }
        }

        fn create_slot_node(&mut self) -> TestNode {
            TestNode { tag: "slot".to_string() }
        }

        fn insert_node(&mut self, _parent: &TestNode, _node: TestNode, _anchor: Option<&TestNode>) {}

        fn insert<T>(&mut self, _parent: &TestNode, _accessor: fn() -> T, _marker: Option<&TestNode>, _initial: Option<T>) -> TestNode {
            TestNode { tag: "inserted".to_string() }
        }

        fn spread<T>(&mut self, _node: &TestNode, _accessor: fn() -> T, _skip_children: Option<bool>) {}

        fn set_property<T>(&mut self, _node: &TestNode, _name: &str, _value: T, _prev: Option<T>) {}

        fn set_prop<T>(&mut self, _node: &TestNode, _name: &str, _value: T, _prev: Option<T>) -> T { _value }

        fn merge_props(&mut self, _sources: &[&dyn std::any::Any]) -> Box<dyn std::any::Any> {
            Box::new(())
        }

        fn use_<A, T>(&mut self, _f: fn(&TestNode, A) -> T, _element: &TestNode, _arg: A) -> T {
            panic!("use_ not implemented in test")
        }
    }

    #[test]
    fn test_renderer_create_element() {
        let mut renderer = TestRenderer;
        let element = renderer.create_element("box");
        assert_eq!(element.tag, "box");
    }

    #[test]
    fn test_renderer_create_text() {
        let mut renderer = TestRenderer;
        let text = renderer.create_text_node("label");
        assert_eq!(text.tag, "text:label");
    }

    #[test]
    fn test_renderer_create_component() {
        fn my_comp(_props: i32) -> TestNode {
            TestNode { tag: "comp".to_string() }
        }
        let node = TestRenderer::create_component(my_comp, 42);
        assert_eq!(node.tag, "component");
    }

    #[test]
    fn test_renderer_render_cleanup() {
        let mut renderer = TestRenderer;
        fn code() -> TestNode {
            TestNode { tag: "root".to_string() }
        }
        let root = renderer.create_element("root");
        let cleanup = renderer.render(code, root);
        cleanup();
    }

    #[test]
    fn test_renderer_set_prop_identity() {
        let mut renderer = TestRenderer;
        let node = renderer.create_element("test");
        let result = renderer.set_prop::<i32>(&node, "value", 99, None);
        assert_eq!(result, 99);
    }
}
