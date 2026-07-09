//! Terminal screen model for terminal output parsing and display.
//!
//! Ported from wezterm's terminal model crates.
//! Provides escape sequence parsing, cell/surface model, and terminal state machine.

#![allow(
    clippy::default_numeric_fallback,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::option_if_let_else,
    clippy::too_long_first_doc_paragraph,
    clippy::needless_pass_by_ref_mut,
    clippy::missing_const_for_fn,
    clippy::use_self,
    clippy::panic,
    clippy::branches_sharing_code,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::derive_partial_eq_without_eq,
    clippy::equatable_if_let,
    clippy::unnecessary_struct_initialization,
    clippy::or_fun_call,
    clippy::module_name_repetitions,
    clippy::suspicious_operation_groupings,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::borrowed_box,
    clippy::should_implement_trait,
    clippy::cognitive_complexity
)]

pub mod bidi;
pub mod char_props;
pub mod color_types;
pub mod input_types;

pub mod cell;
pub mod core;
pub mod escape_parser;
pub mod surface;
