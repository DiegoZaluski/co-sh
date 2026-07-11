//! Harness — agent loop runtime.
//!
//! Bridges LLM responses to tool execution through three tiers:
//! internal harness tools, local cosh-tools, and external MCP servers.

pub mod correction_memory;
pub mod core;
pub mod events;
pub mod tools;

#[cfg(test)]
mod test;

pub use core::{Harness, Mode};
pub use events::HarnessEvent;
pub use tools::{CoshTools, Tools};
