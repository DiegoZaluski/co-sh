//! Harness — agent loop runtime.
//!
//! Bridges LLM responses to tool execution through three tiers:
//! internal harness tools, local cosh-tools, and external MCP servers.

pub mod context_manager;
pub mod core;
pub mod correction_memory;
pub mod events;
pub mod guardrails;
pub mod hooks;
pub mod title;
pub mod tools;
pub mod truncate;

#[cfg(test)]
mod test;

pub use context_manager::{ContextDisplayInfo, ContextManager, ContextManagerState};
pub use core::{Harness, Mode};
pub use events::HarnessEvent;
pub use guardrails::{PermissionAction, PermissionCheck, PermissionRequest, check_tool_permission};
pub use hooks::{AggregateResult as HookAggregateResult, HookConfig, HookDecision, HookRunner};
pub use title::generate_title;
pub use tools::{CoshTools, Tools};
