use thiserror::Error;

/// Failures of the MCP client module. Payloads carry the server or tool name
/// plus the underlying cause, so the TUI can render them directly.
///
/// Transport sources (`io`, `ServiceError`) are folded into messages on
/// purpose: timeouts are discriminated by the manager via `tokio::time`
/// before conversion, and there is no retry policy to feed yet. If retries
/// arrive, revisit with structured sources instead of string matching.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP server '{0}' has an invalid configuration: {1}")]
    InvalidConfig(String, String),

    #[error("Cannot connect to MCP server '{0}': {1}")]
    Connect(String, String),

    #[error("MCP server '{0}' failed to list tools: {1}")]
    ListTools(String, String),

    #[error("No MCP server provides tool '{0}'")]
    UnknownTool(String),

    #[error("MCP tool '{0}' failed: {1}")]
    Call(String, String),

    #[error("MCP server '{0}' timed out after {1}ms")]
    Timeout(String, u64),
}
