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

    #[error("No MCP server provides resource '{0}'")]
    UnknownResource(String),

    #[error("No MCP server provides prompt '{0}'")]
    UnknownPrompt(String),

    #[error("MCP tool '{0}' failed: {1}")]
    Call(String, String),

    #[error("MCP resource '{0}' failed: {1}")]
    ReadResource(String, String),

    #[error("MCP prompt '{0}' failed: {1}")]
    GetPrompt(String, String),

    #[error("MCP server '{0}' timed out after {1}ms")]
    Timeout(String, u64),

    /// The cached protocol-era assumption for this server broke: a connection
    /// attempt in the assumed era was refused with era evidence, so the
    /// caller retries once in the other era. Only surfaces to the user when
    /// the retry fails too (see `era_loop_failed` in the manager) or via a
    /// failure snapshot.
    #[error("MCP server '{0}' rejected a {1} connection: {2}")]
    EraStale(String, &'static str, String),

    /// The server never answered the era probe (`server/discover`) within
    /// the probe budget. Whether this means "legacy server" is a per-transport
    /// policy: the spec's stdio binding treats a probe timeout as a legacy
    /// signal, the HTTP binding does not.
    #[error("MCP server '{0}' did not answer the protocol probe within {1}ms")]
    ProbeTimedOut(String, u64),
}
