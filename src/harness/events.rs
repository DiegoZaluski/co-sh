use cosh_tools::question::types::QuestionItem;
use serde_json::Value;

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
    /// The agent loop finished normally (no more tool calls).
    Done {
        /// Bincode-serialized ContextManagerState for persistence.
        context_state: Vec<u8>,
    },
    /// The agent loop was interrupted by a stop request.
    Stopped {
        /// Bincode-serialized ContextManagerState for persistence.
        context_state: Vec<u8>,
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
    /// A fatal error occurred.
    Error(String),
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
        info: super::context_manager::ContextDisplayInfo,
    },
}
