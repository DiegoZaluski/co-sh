//! Shared result/event types of the harness core.

/// One event delivered to a streaming callback: a text token, or the reset
/// marker the SDK emits when it retries a mid-stream failure (the consumer
/// must drop any partial content from the failed attempt before the retried
/// response restarts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A text token.
    Token(String),
    /// The retried response is about to restart from the beginning.
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Ask,
    Yolo,
    /// Direct-shell mode: the TUI becomes a plain terminal. Every prompt
    /// input is dispatched straight to the bash tool — NO LLM call is ever
    /// made. The command and its output are recorded in the context manager
    /// as HIDDEN items ([`super::context::ContextItem::UserCommand`]): they
    /// persist to the session JSONL for the user, but the model never sees
    /// them (a future feature may expose them; for now the TUI is simply a
    /// terminal and the harness loop never runs in this mode).
    Command,
}

/// Result of the user-triggered `/compact`
/// ([`Harness::compact_on_demand`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualCompactionOutcome {
    /// The deterministic funnel ran and the LLM summary was applied.
    Compacted,
    /// Nothing to fold: the timeline holds at most a lone previous summary.
    NothingToCompact,
    /// The summarizer call failed (the automatic path already toasted).
    Failed,
}
