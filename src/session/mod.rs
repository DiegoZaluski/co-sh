//! Session persistence: the conversation domain model, its event history,
//! the JSONL store and the rebuildable view snapshots.
//!
//! Split out of the TUI: the harness consumes these types directly, so the
//! conversation data model belongs to the library, not to an interface
//! layer.

pub mod history;
pub mod snapshots;
pub mod store;
