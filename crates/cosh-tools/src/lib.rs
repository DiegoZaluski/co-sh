pub mod bash;
pub mod find;
pub mod fs;
pub mod plan;
pub mod skills;
pub mod util;
pub mod vision;
pub mod web;

/// MCP Tool description: name, description, and inputSchema as a JSON value.
///
/// Follows the [MCP Tool schema](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
/// so it can be passed directly to an rmcp server in the future.
/// Structure: `{ "name": string, "description": string, "inputSchema": { ... } }`
pub type ToolDescription = serde_json::Value;
