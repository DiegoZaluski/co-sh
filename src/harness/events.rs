use serde_json::Value;

/// Represents a model entry with its provider
#[derive(Debug, Clone)]
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
    Done,
    /// The agent loop was interrupted by a stop request.
    Stopped,
    /// A fatal error occurred.
    Error(String),
    /// Models list loaded from the provider.
    ModelsLoaded { models: Vec<ModelEntry>, current: String },
}
