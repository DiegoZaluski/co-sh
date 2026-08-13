//! Types for the `subagent.call` tool.

use serde::{Deserialize, Serialize};

/// Input for calling a sub-agent CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallInput {
    /// The agent CLI to call (e.g. "opencode").
    /// Must be one of the supported agents listed in the tool description.
    pub agent: String,
    /// The message to send to the sub-agent.
    ///
    /// Optional: if omitted (or empty), the last message sent to a sub-agent
    /// in this session is reused automatically. If no sub-agent has been
    /// called yet, an error is returned telling the caller to provide one.
    pub input: Option<String>,
}

/// Output from calling a sub-agent CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Exit code of the process.
    pub exit_code: i32,
}
