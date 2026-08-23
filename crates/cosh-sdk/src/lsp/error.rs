//! Error types for the LSP engine.

use std::fmt;

/// Errors surfaced by the [`lsp`](crate::lsp) engine.
///
/// The variants cover every failure mode of the protocol stack: framing,
/// transport I/O, request lifecycle (timeout, cancellation) and JSON-RPC error
/// objects reported by the language server itself.
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// Underlying stream I/O failed.
    #[error("LSP stream I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The server closed its stdout (or the process died), ending the session.
    ///
    /// Every in-flight request is failed with this variant; see
    /// [`Transport`](crate::lsp::Transport)::`exited` for the terminal signal.
    #[error("language server stream closed")]
    StreamClosed,

    /// A frame could not be decoded: missing `Content-Length`, non-UTF-8
    /// body, or a header block that never terminated.
    #[error("invalid LSP frame: {0}")]
    InvalidFrame(String),

    /// A frame announced a `Content-Length` beyond the safety limit. Treated
    /// as a corrupt stream rather than an allocation opportunity.
    #[error("LSP frame of {size} bytes exceeds the {max} byte limit")]
    FrameTooLarge {
        /// Announced body size in bytes.
        size: usize,
        /// Configured maximum accepted frame size in bytes.
        max: usize,
    },

    /// The server did not answer within the configured request timeout.
    ///
    /// The pending entry is dropped; when the response eventually arrives it
    /// is discarded. Dropping the owning future additionally emits
    /// `$/cancelRequest` so the server can stop working on it.
    #[error("timed out waiting for response to `{method}`")]
    Timeout {
        /// Method name of the request that timed out.
        method: Box<str>,
    },

    /// The server answered a request with a JSON-RPC error object.
    #[error("language server error {code}: {message}")]
    Rpc {
        /// JSON-RPC / LSP error code.
        code: i64,
        /// Human-readable message from the server.
        message: String,
        /// Optional structured payload accompanying the error.
        data: Option<serde_json::Value>,
    },

    /// The outbound channel is closed — the transport already terminated
    /// (stream death, writer task gone) or was never alive.
    #[error("language server is no longer running")]
    NotRunning,

    /// The outbound queue hit its capacity limit because the server stopped
    /// reading stdin. Requests already in flight still honor their deadlines;
    /// new traffic should back off or tear the session down.
    #[error("outbound queue is full (server stopped reading stdin?)")]
    Backpressure,

    /// The client was used in a lifecycle state that does not allow the
    /// operation (e.g. `initialize` twice, query after shutdown).
    #[error("invalid client state: {0}")]
    InvalidState(&'static str),

    /// A language-server process could not be brought up. `detail` carries
    /// the spawn/handshake failure description.
    #[error("failed to start language server `{server}`: {detail}")]
    Spawn {
        /// Catalog name of the server.
        server: Box<str>,
        /// Failure description.
        detail: String,
    },

    /// The server is temporarily unavailable: its key is in the post-failure
    /// backoff window, or the binary is not on `PATH`. Retrying after the
    /// backoff (or installing the binary) resolves it.
    #[error("language server `{0}` is unavailable right now")]
    Unavailable(Box<str>),

    /// An incoming message could not be parsed as JSON-RPC.
    ///
    /// Never fatal: the offending frame is logged and skipped so a single bad
    /// message cannot tear down an otherwise healthy session.
    #[error("malformed LSP message: {0}")]
    MalformedMessage(String),
}

/// Terminal condition of a transport, broadcast through
/// [`Transport::exited`](crate::lsp::Transport::exited).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The server closed its stdout. Normal termination path.
    StreamClosed,
    /// Writing to the server's stdin failed; the pipe is broken.
    WriteFailed(String),
    /// The byte stream became untrustworthy (oversized frame, invalid
    /// framing, non-UTF-8 body) and the session ended defensively.
    Protocol(String),
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamClosed => write!(f, "stream closed"),
            Self::WriteFailed(detail) => write!(f, "stdin write failed: {detail}"),
            Self::Protocol(detail) => write!(f, "protocol violation: {detail}"),
        }
    }
}
