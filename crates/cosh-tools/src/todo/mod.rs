//! TODO task management tools for LLM agents.
//!
//! Provides a state-machine-driven todo list with boolean tags,
//! dependency tracking, timeline enforcement, and first-class
//! support for capturing important completed tasks as structured
//! resolutions.
//!
//! The tool returns `Err()` with correction prompts when the LLM
//! violates invariants (e.g. missing rationale on important tasks,
//! concurrent in-progress items, unsatisfied dependencies).
//!
//! # Resolution lifecycle
//!
//! 1. A task is created with `tags.possibly_important = true`.
//! 2. On completion, a non-empty `rationale` is required.
//! 3. The tool nags the LLM to obtain **human confirmation**.
//! 4. After the user confirms, the LLM calls `ConfirmResolution` to
//!    produce a [`PendingResolution`] in the output.
//! 5. The caller decides where/how to persist the record.
//!
//! Nothing is persisted automatically by the tool.

pub mod todowrite;
pub mod types;

#[cfg(test)]
mod test;

pub use todowrite::todowrite;
pub use types::{
    Nag, NagSeverity, PendingResolution, TodoAction, TodoItem, TodoList, TodoStatus, TodoTags,
    TodoWriteOutput,
};
