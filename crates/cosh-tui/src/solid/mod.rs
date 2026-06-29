//! SolidJS-style reactive rendering layer.
//!
//! ## Module tree
//! ```text
//! solid/
//! ├── elements/          — Component hooks and event callbacks
//! │   └── test/
//! ├── renderer/          — SolidJS renderer
//! │   └── test/
//! ├── types/             — ElementProps, marker traits
//! ├── utils/             — id_counter, log
//! ├── reconciler.rs      — DOM node insertion/removal/replacement
//! ├── scrollback.rs      — Scrollback buffer writer
//! ├── time_to_first_draw.rs — TTFD metrics
//! └── test/              — Integration tests
//! ```

pub mod elements;
pub mod reconciler;
pub mod renderer;
pub mod scrollback;
pub mod time_to_first_draw;
pub mod types;
pub mod utils;

#[cfg(test)]
pub mod test;
