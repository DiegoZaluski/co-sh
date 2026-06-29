//! SolidJS-style reactive rendering layer.
//!
//! ## Module tree
//! ```text
//! solid/
//! ├── elements/          — Component hooks, widgets, and catalogue
//! │   ├── catalogue.rs   — Component registry (Span, BR, Link, etc.)
//! │   ├── extras.rs      — Dynamic renderable
//! │   ├── hooks.rs       — Event callbacks
//! │   ├── slot.rs        — Slot placeholder system
//! │   └── test/
//! ├── plugins/           — Slot registry and plugin integration
//! │   └── slot.rs
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
pub mod plugins;
pub mod reconciler;
pub mod renderer;
pub mod scrollback;
pub mod time_to_first_draw;
pub mod types;
pub mod utils;

#[cfg(test)]
pub mod test;
