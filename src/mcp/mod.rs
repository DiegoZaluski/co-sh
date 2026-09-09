//! MCP client module.
//!
//! Provides MCP tool support over `stdio` and Streamable HTTP, configured
//! through `setup.json`, with footer-only status reporting. Resources, prompts,
//! tasks, subscriptions, and OAuth are out of scope for now. Protocol
//! conversions live in [`bridge`] so session management can reuse them
//! independently.
//!
//! ## Protocol eras (MCP 2026-07-28)
//!
//! MCP 2026-07-28 (SEP-2575) removed the `initialize` handshake. Modern
//! servers expose version, identity, and capabilities as per-request metadata
//! and advertise themselves through `server/discover`. Legacy servers
//! (2025-11-25 and earlier) still require `initialize`, which must never be
//! sent to a modern-only server.
//!
//! [`McpManager`] supports both eras and selects the protocol per server:
//!
//! - On the first connection, it probes with `server/discover`. A correlated
//!   non-modern rejection falls back to the legacy handshake. A probe timeout
//!   also triggers fallback over stdio, where silence is legacy evidence; over
//!   HTTP, a timeout is treated as a typed failure instead.
//! - The detected era is cached in memory by server name, allowing reconnects
//!   to skip probing. The cache is never persisted.
//! - If a cached era is later rejected, the manager retries once using the
//!   other era to handle upgraded or replaced servers. If both attempts fail,
//!   the cache is cleared so the next connection probes again.
//!
//! The private [`era`] module owns protocol-era details: the [`Era`] type,
//! the explicit `2026-07-28` version pin, connected-peer era detection, and
//! modern-error classification. The explicit pin avoids rmcp's `LATEST`,
//! which still targets `2025-11-25` in rmcp 3.2.0.
//!
//! [`manager`] owns the probe and retry policy and uses a fresh transport for
//! each attempt. This is intentional: rmcp's `Auto` lifecycle retries on the
//! same session, which rmcp servers may poison after receiving a modern opener.
//! See [`manager`] for the implementation details.

pub mod bridge;
pub mod config;
mod era;
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
