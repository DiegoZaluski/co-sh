//! Core rendering primitives and widget implementations.
//!
//! ## Module tree
//! ```text
//! core/
//! ├── lib/               — Shared utilities (border, rgba, terminal_palette)
//! │   └── test/
//! ├── renderables/       — Widget implementations (Text, Box, Input, Textarea, ScrollBox)
//! │   └── test/
//! ├── renderable.rs      — RenderableNode, RootRenderable, Renderable trait
//! ├── renderer.rs        — Ratatui-backed renderer
//! ├── syntax_style.rs    — Syntax highlighting styles
//! ├── types.rs           — RenderContext, Selection, TextAttributes
//! ├── utils.rs           — Core utilities
//! └── test/              — Integration tests
//! ```

pub mod lib;
pub mod renderables;
pub mod renderable;
pub mod renderer;
pub mod syntax_style;
pub mod types;
pub mod utils;

pub use lib::{border, rgba, terminal_palette};
pub use renderables::{text, text_node, r#box, scroll_box, input, textarea};

#[cfg(test)]
pub mod test;
