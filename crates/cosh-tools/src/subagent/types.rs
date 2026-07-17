//! Types for the `subagent.call` tool.

use serde::{Deserialize, Serialize};

/// Input for calling a sub-agent CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallInput {
    /// The agent CLI to call (e.g. "opencode").
    /// Must be one of the supported agents listed in the tool description.
    pub agent: String,
    /// The message to send to the sub-agent.
    pub input: String,
}

/// Output from calling a sub-agent CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Exit code of the process.
    pub exit_code: i32,
}
