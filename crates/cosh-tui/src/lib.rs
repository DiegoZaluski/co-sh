#![allow(
    clippy::cast_possible_truncation,
    clippy::explicit_counter_loop,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::redundant_closure_for_method_calls,
    dead_code
)]

pub mod core;
pub mod solid;

pub use core::border::BorderStyle;
pub use core::renderables::r#box::BoxRenderable;
pub use core::renderables::input::InputRenderable;
pub use core::renderables::scroll_box::ScrollBoxRenderable;
pub use core::renderables::text::TextRenderable;
pub use core::renderables::text_node::TextNodeRenderable;
pub use core::renderables::textarea::TextareaRenderable;
pub use core::renderable::{Renderable, RenderableNode, RootRenderable};
