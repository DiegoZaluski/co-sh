use cosh_sdk::connector::TokenUsage;
use cosh_tools::question::types::QuestionItem;
use serde_json::Value;

use crate::mcp::ServerSnapshot;

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
    /// Identity emitted by the context producer after the corresponding live
    /// display event. A result links to the item that opened its tool call.
    ContextItemRecorded {
        item_id: u64,
        user: bool,
        call_item_id: Option<u64>,
        tool_name: Option<String>,
        compaction: bool,
    },
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
    ToolCall {
        tool: String,
        input: Value,
        /// Per-call routing identity: the provider's tool-call id (or a
        /// synthetic one for inline calls). Streams carry the SAME ref
        /// back, so the TUI can bind every sub-agent window to the EXACT
        /// call that spawned it — parallel sub-agents (same or different
        /// CLIs) each get their own window traffic. `None` only for
        /// callers outside the agent loop's dispatch (command mode).
        call_ref: Option<String>,
    },
    /// Successful result of a dispatched tool call.
    /// Ordered sequentially — the Nth `ToolResult` matches the Nth `ToolCall`.
    ToolResult {
        output: String,
        /// The dispatching call's routing identity (see `ToolCall`): binds
        /// a background spawn receipt to ITS call's window.
        call_ref: Option<String>,
    },
    /// A tool call that failed during dispatch.
    /// Ordered sequentially — the Nth `ToolError` matches the Nth `ToolCall`.
    ToolError {
        error: String,
        /// The dispatching call's routing identity (see `ToolCall`).
        call_ref: Option<String>,
    },
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
    /// Snapshot of the MCP servers for the TUI's prompt footer. Emitted at
    /// agent-loop start (after the boot `connect_all`, so the first view
    /// already reflects the connections). The TUI reduces it to
    /// ready/failed counts; per-server detail stays available for panels.
    McpStatus {
        /// One snapshot per registered server, in config order.
        servers: Vec<ServerSnapshot>,
    },
    /// The agent loop finished normally (no more tool calls).
    Done {
        /// Context manager state for persistence (the JSONL session log).
        context: ContextManagerState,
        /// The decision model's audit of this stop, when a `Checkup` is
        /// attached: the verdict recorded at a heuristic guard stop, so an
        /// improper stop is never silent. `None` when the stop is explicit
        /// (stop tool, Esc), natural-completion-approved, or unaudited.
        checkup_verdict: Option<String>,
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
        /// The sub-agent CLI this output belongs to (`subagent_call`
        /// mirrors only): the TUI routes the output to THAT agent's
        /// right-panel window. Parallel background sub-agents (ACP,
        /// `run_in_background: true`) each have a Running window, so a
        /// "last Running window" choice would land the report in a
        /// SIBLING's window — both leak. `None` everywhere else (bash,
        /// find, `subagent_status` notifications — those self-identify
        /// through their wrapper text) and whenever no routing hint is
        /// known: the TUI keeps its last-Running default.
        agent: Option<String>,
        /// Per-call routing identity (see `ToolCall`): binds this stream/
        /// report to the EXACT dispatching call. The TUI prefers this over
        /// the `agent` hint — two parallel calls of the SAME agent CLI
        /// share the hint but never the ref, so an output can never land
        /// in a sibling's window (the input1↔output1 cross-pairing bug).
        call_ref: Option<String>,
    },
    /// A typed progress event from a running ACP sub-agent
    /// ([`cosh_tools::subagent::events::SubagentEvent`]). Message-text
    /// events are ALSO mirrored through [`HarnessEvent::ToolOutput`] so the
    /// existing chat rendering does not regress; this variant carries the
    /// rest (thoughts, tool calls, plans, usage) for the TUI's sub-agent
    /// box (Phase 3).
    SubagentEvent {
        /// The tool call the event belongs to (always `subagent_call`).
        tool: String,
        /// Per-call routing identity (see `ToolCall`): the live mini-chat
        /// timeline (thoughts, tool rows, plans) belongs to ONE call's
        /// window — without it, parallel sub-agents would interleave each
        /// other's activity lines.
        call_ref: Option<String>,
        /// The typed event.
        event: cosh_tools::subagent::events::SubagentEvent,
    },
    /// One live event of the `/agents` split-pane mode. Each pane is an
    /// independent ACP (or nested cosh) turn driven directly by the TUI —
    /// outside the agent loop's `subagent_call` dispatch — so pane index
    /// replaces `call_ref` as the routing identity: the pane's own event
    /// channel is the multiplexer the harness tool path cannot provide.
    /// `epoch` is the dispatch-time generation of the split slots (`App::
    /// agents_pane_epoch`): a slot swap (fresh confirm, park, switch, close)
    /// bumps it, and the handler drops every event of a STALE epoch — a
    /// stopped turn's terminal delivery can never land in a replacement
    /// pane that merely shares the index.
    AgentsPaneEvent {
        /// Index into `App::agents_split`'s panes (0 = the `cosh` manager pane).
        pane: usize,
        /// Stable identity of the pane object this turn was dispatched to
        /// (`AgentPane::id`). The event handler routes by THIS, not the
        /// index: a pane set rebuilt around a still-running turn (a fresh
        /// picker confirm that keeps the pane) keeps the pane's id, so the
        /// stream follows the pane to its new index — and a pane the
        /// rebuild dropped carries an id no pane answers to, so its events
        /// are dropped instead of corrupting a replacement's timeline.
        /// `0` = never assigned (tests / unstamped senders); those fall
        /// back to the epoch + index gate.
        pane_id: u64,
        /// Dispatch-time split-slot generation (see the variant doc).
        epoch: u64,
        /// The typed event, the same shape the right-panel sub-agent box
        /// renders.
        event: cosh_tools::subagent::events::SubagentEvent,
    },
    /// A `/agents` pane turn ENDED. Carries the final report (the
    /// `TurnClosure` contract: the last post-tool-call message), the stop
    /// reason and the session id for the pane's next resume. `epoch` is the
    /// dispatch-time split-slot generation: a stale Done (the pane's set
    /// was replaced or torn down while the turn was being cancelled) is
    /// dropped — its session id must never bind into a replacement pane or
    /// a different session's metadata.
    AgentsPaneDone {
        /// Index into `App::agents_split`'s panes.
        pane: usize,
        /// Stable identity of the pane object the turn ran in (see
        /// `AgentsPaneEvent::pane_id`).
        pane_id: u64,
        /// Dispatch-time split-slot generation (see the variant doc).
        epoch: u64,
        /// The final report; an errored turn carries the error text instead.
        output: String,
        /// `end_turn`, `cancelled`, or a spawn-failure marker.
        stop_reason: String,
        /// The ACP session id to resume on the pane's next prompt (external
        /// agents only; the nested cosh manager pane has no resumable session).
        session: Option<String>,
    },
    /// A `/agents` pane's cold-restore REPLAY ended. The pane rehydrated
    /// its timeline from the agent's own `session/load` history replay
    /// (the `AgentsPaneEvent`s that preceded this one) — nothing was
    /// prompted, nothing changed in the agent's context. `ok` is false
    /// when the replay pass failed (stale id, harness without
    /// `loadSession`, spawn failure): the pane keeps its (empty) live
    /// timeline and shows the reason in the mirror.
    AgentsPaneReplayed {
        /// Index into `App::agents_split`'s panes.
        pane: usize,
        /// Stable identity of the pane object the replay runs for (see
        /// `AgentsPaneEvent::pane_id`).
        pane_id: u64,
        /// Dispatch-time split-slot generation (see the variant doc).
        epoch: u64,
        /// Whether the replay completed (false = the failure reason).
        ok: bool,
        /// The failure reason (empty on success).
        error: String,
    },
    /// Passive LSP findings collected by an fs operation after it touched a
    /// file. The model sees the same findings inline in the tool result; this
    /// event carries the user-facing rendering signal: the TUI shows errors
    /// in red and warnings in yellow below the tool's code/diff block.
    ToolDiagnostics {
        /// The fs tool that produced the findings (e.g. `fs_edit`).
        tool: String,
        /// Structured findings, already settled and severity-split.
        notes: cosh_tools::fs::LspNotes,
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
        /// The decision model's audit of this stop (same contract as
        /// [`HarnessEvent::Done::checkup_verdict`]).
        checkup_verdict: Option<String>,
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
    /// The main loop has entered (or left) its pre-Done busy-guard wait for
    /// `/agents` pane reports.
    ///
    /// The loop is `Working` throughout — it is not Idle — but it is IDLE in
    /// the only sense the user cares about: it finished its own work and is
    /// merely waiting for a report to come back. The TUI needs that
    /// distinction to decide what a submitted prompt means: in this state the
    /// message goes straight to the agent (the harness's wait wakes on it)
    /// rather than into a queue the user would be right to call unnecessary.
    ///
    /// `false` is emitted as soon as the wait ends, whatever ended it, so the
    /// flag can never outlive the condition it describes.
    AwaitingPaneReports { awaiting: bool },
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
    /// A one-off, non-streaming correction of the editable prompt. This is
    /// separate from the agent-loop transcript: its result only replaces the
    /// prompt draft when that draft still matches `original`.
    PromptCorrection {
        /// Prompt text as it was when the correction was requested.
        original: String,
        /// Corrected text, or the provider/request error to surface as a
        /// TUI toast while preserving the original draft.
        result: Result<String, String>,
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
        /// (`usage.cost` — OpenRouter, Charm Hyper, OpenCode Zen/Go). The
        /// authoritative billed amount and the ONLY price the TUI shows:
        /// there is no local estimate fallback. `None` when the provider
        /// does not report a cost — the record stays unpriced (excluded
        /// from dollar totals; its tokens still count).
        reported_cost: Option<f64>,
        /// REAL cost in the provider's NATIVE prepaid unit when that is not
        /// USD (Charm Hyper: `usage.cost.hypercredits`), parallel to
        /// `reported_cost` — both come from the same `usage.cost` object.
        /// Internal bookkeeping (session spend sums); NOT what the header
        /// renders. `None` for providers that bill natively in USD.
        reported_cost_credits: Option<f64>,
        /// Account's REMAINING balance in the provider's native prepaid
        /// unit (Charm Hyper: `usage.remaining.hypercredits`) as of the
        /// LAST completed request. The user-facing ◆ figure: it decreases
        /// as the user spends and matches what their Charm account shows.
        /// `None` for providers without a prepaid balance.
        remaining_credits: Option<f64>,
    },
    /// The weights-installer lifecycle, emitted by the agent loop around the
    /// first-use download of the decision-model mirror. The TUI streams the
    /// explanation into the chat as a REAL assistant message (it enters the
    /// agent context) and mirrors the download progress as a chat-line bar.
    WeightsInstall {
        /// Which stage of the install this event reports.
        event: WeightsInstallEvent,
    },
}

/// A stage of the weights installer ([`HarnessEvent::WeightsInstall`]).
#[derive(Debug, Clone)]
pub enum WeightsInstallEvent {
    /// The OPENING chunk of the streamed explanation. The TUI opens a real
    /// assistant message with it (`synthetic: false`): the harness adds
    /// the FULL text to the agent context once the install succeeds, so
    /// the model can explain/translate it to the user.
    TextBegin(String),
    /// A follow-up chunk of the streamed explanation: the TUI appends it
    /// to the message [`WeightsInstallEvent::TextBegin`] opened. The
    /// token-like pacing happens upstream — the sender sleeps between
    /// chunks, so the text appears the way an LLM streams.
    TextDelta(String),
    /// Download progress: bytes received and bytes known so far (the total
    /// grows as further files are announced). Throttled upstream. Carries
    /// the model's display name — the progress line addresses the model
    /// by it ("installing Laya ▓▓▓…").
    Progress {
        /// The model's user-facing name (`Laya`, `Laya-Multilingual`, …).
        model: String,
        /// Bytes received across every file of this install.
        bytes_done: u64,
        /// Total bytes announced so far.
        bytes_total: u64,
    },
    /// The target was already cached — no download ran. Carries no payload:
    /// the TUI uses it to freeze any orphaned running install line left by
    /// a crashed session (a cache-hit turn emits nothing else).
    Cached,
    /// The install completed and verified; the model can load. Carries the
    /// repo it installed from and the resolved commit SHA — the caller
    /// (the TUI, which owns the setup file) records the SHA as the pin
    /// when the repo is the effective mirror: one revision per mirror
    /// checkpoint, every later load cache-probes exactly that snapshot.
    Finished {
        /// The repo the checkpoint was installed from.
        repo: String,
        /// The commit SHA the artifacts came from.
        revision: String,
    },
    /// The install failed (network, digest mismatch). Fail-open: the turn
    /// proceeds without the decision-model audit.
    Failed(String),
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
