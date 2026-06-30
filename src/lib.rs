//! Cosh harness — the agent loop runtime.
//!
//! # Tool namespace convention
//!
//! For best results, MCP servers should name their tools using the convention
//! `namespace.name` (e.g. `filesystem.read`, `database.query`). The harness
//! groups tools by namespace, caches LLM-generated summaries per namespace,
//! and renders compact headers. Tools without a dot in the name are displayed
//! with full schema inline and do not participate in namespace caching.

pub mod harness;
pub mod namespace_cache;
pub mod summarizer;

#[cfg(test)]
mod test;

pub use harness::Harness;

// .....
pub use cosh_tools::{bash, fs, plan, skills, util, web};
