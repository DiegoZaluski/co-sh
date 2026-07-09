#![allow(
    clippy::multiple_crate_versions, // transitive deps: bitflags, thiserror, etc.
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
pub use core::layout::LayoutTree;
pub use core::renderable::{Renderable, RenderableNode, RootRenderable};
pub use core::renderables::ascii_font::ASCIIFontRenderable;
pub use core::renderables::r#box::BoxRenderable;
pub use core::renderables::diff::DiffRenderable;
pub use core::renderables::input::InputRenderable;
pub use core::renderables::markdown::MarkdownRenderable;
pub use core::renderables::scroll_bar::ScrollBarRenderable;
pub use core::renderables::scroll_box::ScrollBoxRenderable;
pub use core::renderables::select::SelectRenderable;
pub use core::renderables::slider::SliderRenderable;
pub use core::renderables::tab_select::TabSelectRenderable;
pub use core::renderables::text::TextRenderable;
pub use core::renderables::text_node::TextNodeRenderable;
pub use core::renderables::text_table::TextTableRenderable;
pub use core::renderables::textarea::TextareaRenderable;
pub use solid::elements::catalogue::{
    LineBreakRenderable, LinkRenderable, SpanRenderable, create_component, register_component,
};
pub use solid::elements::extras::DynamicRenderable;
pub use solid::elements::slot::{SlotRenderable, TextSlotRenderable};

pub use core::types::{MouseButton, MouseEvent, MouseEventType, MouseModifiers};
