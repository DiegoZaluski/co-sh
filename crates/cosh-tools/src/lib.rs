// Known transitive dependency version duplicates intentionally kept:
// bitflags 1.x/2.x, thiserror 1.x/2.x, phf 0.11/0.13, rand 0.8/0.9, etc.
// These cannot be unified without breaking upstream crates.
#![allow(clippy::multiple_crate_versions)]

pub mod bash;
pub mod find;
pub mod fs;
pub mod lsp;
pub mod plan;
pub mod question;
#[cfg(feature = "embed")]
pub mod recall;
pub mod skills;
pub mod subagent;
pub mod computer;
pub mod util;
pub mod web;

/// MCP Tool description: name, description, and inputSchema as a JSON value.
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

/// Native function-calling instruction for providers that MUST use their
/// structured tool mechanism instead of inline JSON text.
///
/// Gemini 3.x models obey the legacy inline-JSON `TOOL_FORMAT` literally:
/// with it in the prompt they emit tool calls as raw TEXT frames (`{"name":
/// "fs_read", ...}`) even when the request also carries `toolConfig AUTO` —
/// the API then rejects the turn with `MALFORMED_FUNCTION_CALL` and stray
/// fragments (`}`) plus reasoning text leak to the user. The harness selects
/// this variant when the active provider is Gemini (OpenAI/Claude ignore the
/// inline instruction and use their native mechanism regardless).
pub const TOOL_FORMAT_NATIVE: &str = concat!(
    "## Tool format\n",
    "You call tools using the NATIVE function calling mechanism: the platform \n",
    "exposes each tool as a structured functionCall and delivers the result \n",
    "after you invoke it. NEVER write a tool call as JSON inside your text \n",
    "response — no {\"name\": ..., \"arguments\": ...} objects in prose, code \n",
    "blocks or anywhere else. Text output is reserved for answering the user. \n",
    "When you need a tool, invoke it as a native function call; the platform \n",
    "handles the rest.\n"
);
