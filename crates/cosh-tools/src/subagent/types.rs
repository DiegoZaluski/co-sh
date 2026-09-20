//! Types for the `subagent.call` tool.

use serde::{Deserialize, Serialize};

/// Input for calling a sub-agent.
///
/// The same visible tool dispatches to TWO implementations: an external
/// ACP agent harness when `agent` is provided, or the internal sub-agent
/// (a nested harness) when `agent` is omitted or empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallInput {
    /// The agent harness to call (e.g. "gemini").
    /// Must be one of the supported agents listed in the tool description.
    ///
    /// Optional: if omitted (or empty), an internal agent runs the task
    /// instead — a fresh nested harness with an empty context, in
    /// auto-approve mode, that persists nothing and returns only its final
    /// report.
    pub agent: Option<String>,
    /// The message to send to the sub-agent.
    ///
    /// Optional: if omitted (or empty), the last message sent to a sub-agent
    /// in this session is reused automatically. If no sub-agent has been
    /// called yet, an error is returned telling the caller to provide one.
    pub input: Option<String>,
}

/// Output from calling a sub-agent over ACP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Why the prompt turn ended (the ACP stop reason, e.g. `EndTurn`), or
    /// one of the client-side terminal markers `timeout` / `error` when the
    /// harness was torn down or failed mid-turn.
    pub stop_reason: String,
}
