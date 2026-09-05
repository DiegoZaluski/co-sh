use cosh_sdk::connector::TokenUsage;
use cosh_tools::question::types::QuestionItem;
use serde_json::Value;

use super::context::ContextManagerState;

/// Represents a model entry with its provider
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
}

/// Events emitted by [`Harness::run_agent_loop`](super::Harness) for the TUI
/// to consume and render in real time.
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    /// A text token streamed from the LLM.
    Token { text: String },
    /// A NEW streaming attempt is about to start. The SDK's retry middleware
    /// re-streams failed attempts from the beginning; this event scopes what
    /// the TUI may later discard on [`HarnessEvent::ClearAssistant`]: only
    /// the message(s) opened AFTER this boundary belong to the current
    /// attempt — the previous iteration's transcript must never be popped.
    BeginAssistant,
    /// The SDK retried a mid-stream failure and is about to re-stream the
    /// response from the beginning: the TUI must drop any partial assistant
    /// message it rendered from the failed attempt (it would otherwise
    /// concatenate with the retried response).
    ClearAssistant,
    /// A complete tool call extracted from the LLM response.
    ToolCall { tool: String, input: Value },
    /// Successful result of a dispatched tool call.
    /// Ordered sequentially — the Nth `ToolResult` matches the Nth `ToolCall`.
    ToolResult { output: String },
    /// A tool call that failed during dispatch.
    /// Ordered sequentially — the Nth `ToolError` matches the Nth `ToolCall`.
    ToolError { error: String },
    /// A reasoning block from the LLM.
    Reasoning { text: String },
    /// Snapshot of the language-server status for the TUI's prompt footer.
    /// Emitted from the agent loop as LSP state changes. Rendered as colored
    /// language tags (one per active server); `available` tells the TUI
    /// whether LSP is configured on at all, so it can distinguish "no server
    /// running right now" from "LSP disabled" (`COSH_LSP=off`).
    LspServers {
        /// Whether LSP is enabled (`COSH_LSP` not `off`). Independent of how
        /// many servers are currently running.
        available: bool,
        /// Catalog server names currently alive (e.g. `rust-analyzer`),
        /// deduplicated and unsorted.
        servers: Vec<String>,
    },
    /// The agent loop finished normally (no more tool calls).
    Done {
        /// Context manager state for persistence (the JSONL session log).
        context: ContextManagerState,
    },
    /// The agent loop was interrupted by a stop request.
    Stopped {
        /// Context manager state for persistence (the JSONL session log).
        context: ContextManagerState,
    },
    /// Intermediate output from a running tool (e.g. bash streaming).
    ToolOutput {
        /// The tool name that produced this output.
        tool: String,
        /// The output chunk text.
        output: String,
        /// Whether this is the final chunk for this tool call.
        finished: bool,
    },
    /// A fatal error occurred. The harness paths carry the context snapshot
    /// WITH the display-only error item already recorded
    /// ([`ContextManager::add_error`]) so the TUI can persist the styled
    /// error line right away — the loop is over, no `Done` snapshot follows.
    /// `None` when the failure happened outside the harness context (thread
    /// runtime, connector construction, panic): the TUI then records the
    /// error item itself.
    Error {
        message: String,
        context: Option<ContextManagerState>,
    },
    /// Models list loaded from the provider.
    ModelsLoaded {
        models: Vec<ModelEntry>,
        current: String,
    },
    /// The agent wants to ask the user questions. The TUI should show a dialog
    /// and send answers back via the answer channel.
    QuestionRequest { questions: Vec<QuestionItem> },
    /// The harness needs user permission before dispatching a tool.
    /// The TUI should show the permission dialog and send the response
    /// back via the permission channel.
    PermissionRequest {
        tool: String,
        description: String,
        args: String,
    },
    /// Context manager budget info for the TUI header bar.
    /// Sent after each [`ContextManager::run`] cycle during the agent loop.
    ContextInfo {
        /// Current context budget usage info.
        info: super::context::ContextDisplayInfo,
    },
    /// Periodic context snapshot during a long agent run, so the TUI can
    /// persist the session log incrementally. A crash or restart mid-run then
    /// resumes from the latest snapshot instead of the session-start state.
    /// Emitted from the agent loop at a throttled cadence.
    ContextSnapshot {
        /// The context manager state (the JSONL session log).
        context: ContextManagerState,
    },
    /// The LLM compaction (the last-resort fallback) lifecycle. The harness
    /// drives the model call (the context manager is synchronous and LLM-free),
    /// so these events are emitted by the harness around the summarization
    /// call — `Started` before it, `Finished`/`Failed` after.
    LlmCompaction {
        /// Which stage of the LLM compaction this event reports.
        event: LlmCompactionEvent,
    },
    /// A text delta of the LLM-compaction summary, streamed live while the
    /// summarizer model writes it. The TUI accumulates these into the
    /// "Summarizing" box so the user sees the process visually, exactly like
    /// the main agent's tokens stream into the chat.
    LlmCompactionToken {
        /// The next chunk of the summary text.
        text: String,
    },
    /// The user-triggered `/compact` finished (the harness task that ran
    /// [`Harness::compact_on_demand`](super::core::Harness::compact_on_demand)
    /// is done). The TUI clears its re-entry guard and toasts the outcome.
    CompactOnDemand {
        /// How the manual compaction ended.
        outcome: super::core::ManualCompactionOutcome,
    },
    /// A user message queued for the NEXT REQUEST (the TUI's "next
    /// request" queue) was injected into the model context mid-loop. The
    /// harness drains the queued-input channel before every request and adds
    /// each message as a regular protected user turn; the TUI uses this
    /// acknowledgment to move the message out of its pending area into the
    /// normal session history. Emitted in FIFO order.
    UserMessageInjected {
        /// The injected message text.
        text: String,
    },
    /// A user-visible notification, rendered by the TUI through its existing
    /// toast system. Emitted when the context-window overflow could not be
    /// relieved (the split could not fit the context) or a provider error
    /// survived its retries — the session keeps working, but the user must act
    /// (switch the model, start a new session).
    Toast {
        /// The message to display.
        message: String,
        /// Severity/color variant.
        variant: ToastVariant,
    },
    /// The async title generator produced a semantic title for the session.
    /// The TUI updates the in-memory session and sidebar.
    TitleGenerated {
        /// The session whose title was updated.
        session_id: String,
        /// The new human-readable title.
        title: String,
    },
    /// Real token usage reported by the provider for a completed LLM request.
    ///
    /// Emitted once per API call (each agent-loop request, LLM compaction,
    /// etc.) after its stream finishes. Counters come straight from the
    /// provider's own usage object — never a local estimate. The TUI stamps
    /// the current session id + timestamp and persists it for cost tracking.
    Usage {
        /// The normalized per-request token usage (including cache accounting).
        usage: TokenUsage,
        /// The provider that served the request (e.g. `"opencode"`, `"claude"`).
        provider: String,
        /// The model that served the request.
        model: String,
        /// REAL cost (USD) reported by the provider inside its usage object
        /// (`usage.cost` — OpenRouter, Vercel AI Gateway, OpenCode Zen). The
        /// authoritative billed amount: always supersedes the local
        /// price-table estimate. `None` when the provider does not report a
        /// cost (the TUI then falls back to the estimate, or to no cost at
        /// all when no price is known).
        reported_cost: Option<f64>,
    },
}

/// Severity of a [`HarnessEvent::Toast`]. Kept in the harness (not the TUI)
/// so the TUI maps it onto its own toast variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastVariant {
    /// Neutral informational notice.
    Info,
    /// Successful completion notice.
    Success,
    /// A warning the user should act on (e.g. context-window overflow).
    Warning,
    /// An error that survived its retries.
    Error,
}

/// A stage of the LLM compaction (the last-resort fallback), emitted by the
/// harness while it runs the summarization model call so the TUI can show a
/// live line in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmCompactionEvent {
    /// The harness is about to call the model for the continuation summary.
    Started,
    /// Progress through a MapReduce phase. Map outputs are deliberately not
    /// streamed into the final-summary body, so parallel completion cannot
    /// interleave misleading text in the TUI.
    Progress {
        phase: LlmCompactionPhase,
        completed: usize,
        total: usize,
    },
    /// The next streamed tokens are the candidate/final checkpoint rather
    /// than phase status; the TUI clears the transient progress text first.
    OutputStarted,
    /// The summary was produced and applied — the context was compacted.
    Finished,
    /// The summarization call failed or produced no output — the compaction
    /// is skipped this round and the context stays as it was.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmCompactionPhase {
    Mapping,
    SequentialFallback,
    Reducing,
    Validating,
    Correcting,
}
