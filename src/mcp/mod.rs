//! MCP client module.
//!
//! Professional replacement for the provisional prototype that lived in
//! `harness::core` (`ServerSession`, `Harness::connect`). Scope is
//! deliberately narrow: tools over `stdio` and Streamable HTTP, configured
//! in `setup.json`, with footer-only status. Resources, prompts, tasks,
//! subscriptions and OAuth are explicit follow-ups; conversions live in
//! `bridge` so session management can reuse them untouched.

pub mod bridge;
pub mod config;
pub mod error;
pub mod manager;
pub mod types;

pub use bridge::{is_tool_error, result_to_text, tool_to_definition, tool_to_schema};
pub use config::{
    HttpTransport, McpConfig, McpServerEntry, McpTransport, StdioTransport, build_mcp_entry,
    parse_mcp_endpoint, parse_mcp_timeout, validate_mcp_config, validate_mcp_entry,
};
pub use error::McpError;
pub use manager::McpManager;
pub use types::{ServerSnapshot, ServerStatus, failed_count, ready_count};
