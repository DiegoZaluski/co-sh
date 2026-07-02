//! Harness — agent loop runtime.
//!
//! Bridges LLM responses to tool execution through three tiers:
//! internal harness tools, local cosh-tools, and external MCP servers.

pub mod core;
pub mod tools;

#[cfg(test)]
mod test;

pub use core::Harness;
pub use tools::{CoshTools, Tools};
