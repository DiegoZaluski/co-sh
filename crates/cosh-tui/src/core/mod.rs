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
pub use lib::{border, detect_links, rgba, styled_text, terminal_palette};
pub use renderables::{
    ascii_font, r#box, code, diff, input, markdown, scroll_bar, scroll_box, select, slider,
    tab_select, text, text_node, text_table, textarea,
};

#[cfg(test)]
pub mod test;
