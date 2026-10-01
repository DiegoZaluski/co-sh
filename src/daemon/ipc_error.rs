//! Error plumbing for the daemon layer.
//!
//! Kept separate from [`super::ipc`] so the transport module reads as the
//! protocol it implements, not as its error taxonomy. Two error families:
//!
//! - [`Ipc`] — transport/protocol failures on either side of the socket
//!   (framing, parse, closed peer, remote JSON-RPC error object).
//! - [`Lifecycle`] — process management failures in `activation` (spawn,
//!   socket never appeared, binary missing).

/// Transport and protocol failures.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Ipc {
    /// The peer closed the connection between frames (client side: the
    /// daemon is gone; server side: the client hung up mid-protocol).
    #[error("connection closed by peer")]
    Closed,
    /// The frame did not parse as JSON, or as a JSON-RPC 2.0 message.
    #[error("malformed message")]
    Parse,
    /// Structurally invalid — batching, missing id, both result and error.
    #[error("invalid request")]
    InvalidRequest,
    /// Typed params/results failed to deserialize.
    #[error("invalid params")]
    InvalidParams,
    /// A length prefix beyond the frame cap.
    #[error("frame exceeds the size cap")]
    FrameTooLarge,
    /// A response (or the handshake) did not arrive within the call
    /// timeout — enforced via socket timeouts, never by hanging the turn.
    #[error("call timed out")]
    Timeout,
    /// The daemon on the other end is not who we expected (foreign daemon
    /// on our socket path) or speaks a newer protocol than we do.
    #[error("handshake rejected: {0}")]
    Handshake(String),
    /// A local I/O failure on an established connection.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The remote side answered with a JSON-RPC error object.
    #[error("remote error {}: {}", .0.code, .0.message)]
    Remote(super::ipc::RpcError),
    /// Serialization of a typed value failed (never expected; serde_json is
    /// infallible over `Value`-shaped data).
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl Ipc {
    /// The JSON-RPC reserved code a server should reply with. `Remote` is
    /// client-side only and never reaches a responder.
    pub(crate) fn rpc_code(&self) -> i32 {
        use super::ipc::RpcError;
        match self {
            Self::Parse => RpcError::PARSE,
            Self::InvalidRequest => RpcError::INVALID_REQUEST,
            Self::InvalidParams => RpcError::INVALID_PARAMS,
            _ => RpcError::INTERNAL,
        }
    }
}

/// Daemon process lifecycle failures (see [`super::activation`]).
#[derive(Debug, thiserror::Error)]
pub(crate) enum Lifecycle {
    /// The daemon binary is not next to the running app (`current_exe()`
    /// resolution failed or the spawn itself failed).
    #[error("cannot spawn the {name} daemon: {source}")]
    Spawn {
        name: &'static str,
        source: std::io::Error,
    },
    /// The socket did not appear within the activation window.
    #[error("the {name} daemon never became reachable at {path}")]
    Timeout { name: &'static str, path: String },
}
