//! Core rendering primitives and widget implementations.

pub mod layout;
pub mod lib;
pub mod renderable;
pub mod renderables;
pub mod renderer;
pub mod syntax_style;
pub mod types;
pub mod utils;

pub use layout::LayoutTree;
pub use lib::{border, rgba, terminal_palette};
pub use renderables::{
    r#box, input, scroll_bar, scroll_box, select, slider, tab_select, text, text_node, textarea,
};

#[cfg(test)]
pub mod test;
