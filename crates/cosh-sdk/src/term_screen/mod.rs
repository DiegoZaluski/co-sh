//! Terminal screen model for terminal output parsing and display.
//!
//! Ported from wezterm's terminal model crates.
//! Provides escape sequence parsing, cell/surface model, and terminal state machine.

pub mod bidi;
pub mod char_props;
pub mod color_types;
pub mod input_types;

pub mod escape_parser;
pub mod cell;
pub mod surface;
pub mod core;
