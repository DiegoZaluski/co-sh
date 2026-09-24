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
    /// Whether this sub-agent task is a CODE REVIEW.
    ///
    /// When `true`, the harness appends a contract to the prompt: the
    /// sub-agent's FINAL REPORT must start with an HTML comment header
    /// declaring the review outcome — `<!-- severity: green -->`,
    /// `<!-- severity: yellow -->` (minor issues / bad practice at most) or
    /// `<!-- severity: red -->` (something critical was found). The header
    /// is consumed by the client: it is never rendered, it only tints the
    /// sub-agent box (green/yellow/red). Without the flag (or without a
    /// header in the report) the box keeps its neutral per-agent color.
    ///
    /// Optional; defaults to `false`.
    #[serde(default)]
    pub code_review: bool,
    /// Resume the agent's most recent session, keeping its context (default).
    ///
    /// `true` (or omitted): the call resumes the sub-agent's most recent
    /// ACP session for this agent (tracked per agent name in the calling
    /// session), keeping its conversation context — the token-efficient
    /// default for review/iteration loops. `false`: start a brand-new
    /// session with clean context, for tasks unrelated to the previous
    /// one (the `--continue`/`-c` CLI convention, inverted to a positive
    /// flag).
    ///
    /// Graceful degradation: a harness without session-resume support, or
    /// one whose stored session no longer exists (e.g. it restarted and
    /// lost state), silently falls back to a fresh `session/new`.
    ///
    /// Optional; defaults to `true`. Ignored when `agent` is omitted (the
    /// internal agent never persists state across calls).
    #[serde(default = "default_true")]
    pub continue_session: bool,
}

/// Serde default for [`SubAgentCallInput::continue_session`]: omitted means
/// RESUME (the token-efficient default), so the flag is opt-out only.
fn default_true() -> bool {
    true
}

/// Output from calling a sub-agent over ACP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Why the prompt turn ended: a snake_case ACP stop reason (e.g.
    /// `end_turn`, `cancelled` — the spec-mandated answer to a
    /// `session/cancel`), or the client-side terminal marker `error` when
    /// the harness failed mid-turn. There is no timeout: the turn runs
    /// until the agent ends it or the user stops it.
    pub stop_reason: String,
}
