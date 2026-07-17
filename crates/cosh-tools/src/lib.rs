// Known transitive dependency version duplicates intentionally kept:
// bitflags 1.x/2.x, thiserror 1.x/2.x, phf 0.11/0.13, rand 0.8/0.9, etc.
// These cannot be unified without breaking upstream crates.
#![allow(clippy::multiple_crate_versions)]

pub mod bash;
pub mod find;
pub mod fs;
pub mod plan;
pub mod question;
pub mod skills;
pub mod subagent;
pub mod util;
pub mod vision;
pub mod web;

/// MCP Tool description: name, description, and inputSchema as a JSON value.
///
/// Follows the [MCP Tool schema](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
/// so it can be passed directly to an rmcp server in the future.
/// Structure: `{ "name": string, "description": string, "inputSchema": { ... } }`
pub type ToolDescription = serde_json::Value;

pub const TOOL_FORMAT: &str = concat!(
    "## Tool format\n",
    "To call a tool, respond with a JSON object:\n",
    "{\"name\": \"tool_name\", \"arguments\": { ... }}\n\n",
    "Warning: Do not wrap tool calls in a code block or any markdown formatting. Emit the tool call as raw, unformatted JSON only — no backticks, no language tags, no surrounding text.\n\n",
    "INCORRECT (do not do this):\n",
    "```json\n",
    "    {\"name\": \"tool_name\", \"arguments\": {...}}\n",
    "```\n\n",
    "CORRECT:",
    "{\"name\": \"tool_name\", \"arguments\": {...}}\n"
);
