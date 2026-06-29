pub trait RendererOptions {
    type Node;

    fn create_element(&mut self, tag: &str) -> Self::Node;
    fn create_text_node(&mut self, value: &str) -> Self::Node;
    fn create_slot_node(&mut self) -> Self::Node;
    fn replace_text(&mut self, text_node: &Self::Node, value: &str);
    fn is_text_node(&self, node: &Self::Node) -> bool;
    fn set_property<T>(&mut self, node: &Self::Node, name: &str, value: T, prev: Option<T>);
    fn insert_node(&mut self, parent: &Self::Node, node: Self::Node, anchor: Option<&Self::Node>);
    fn remove_node(&mut self, parent: &Self::Node, node: &Self::Node);
    fn get_parent_node(&self, node: &Self::Node) -> Option<&Self::Node>;
    fn get_first_child(&self, node: &Self::Node) -> Option<&Self::Node>;
    fn get_next_sibling(&self, node: &Self::Node) -> Option<&Self::Node>;
}

pub trait Renderer<Node> {
    fn render(&mut self, code: fn() -> Node, node: Node) -> Box<dyn FnOnce()>;
    fn effect<T>(&mut self, f: Box<dyn FnMut(Option<T>) -> T>, init: Option<T>);
    fn memo<T: 'static>(&mut self, f: Box<dyn Fn() -> T>, equal: bool) -> Box<dyn Fn() -> T>;
    fn create_component<T>(comp: fn(T) -> Node, props: T) -> Node;
    fn create_element(&mut self, tag: &str) -> Node;
    fn create_text_node(&mut self, value: &str) -> Node;
    fn create_slot_node(&mut self) -> Node;
    fn insert_node(&mut self, parent: &Node, node: Node, anchor: Option<&Node>);
    fn insert<T>(
        &mut self,
        parent: &Node,
        accessor: fn() -> T,
        marker: Option<&Node>,
        initial: Option<T>,
    ) -> Node;
    fn spread<T>(&mut self, node: &Node, accessor: fn() -> T, skip_children: Option<bool>);
    fn set_property<T>(&mut self, node: &Node, name: &str, value: T, prev: Option<T>);
    fn set_prop<T>(&mut self, node: &Node, name: &str, value: T, prev: Option<T>) -> T;
    fn merge_props(&mut self, sources: &[&dyn std::any::Any]) -> Box<dyn std::any::Any>;
    fn use_<A, T>(&mut self, f: fn(&Node, A) -> T, element: &Node, arg: A) -> T;
}
