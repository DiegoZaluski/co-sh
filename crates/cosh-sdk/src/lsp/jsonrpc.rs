//! JSON-RPC 2.0 framing and message types for the LSP wire protocol.
//!
//! This module is deliberately transport-agnostic: it knows how to turn bytes
//! into [`IncomingMessage`]s and outgoing payloads into framed bytes, nothing
//! more. Routing, timeouts and lifecycle live in
//! [`transport`](super::transport).
//!
//! # Framing quirks tolerated on purpose
//!
//! The LSP spec mandates strict `Content-Length` framing, but real servers
//! occasionally print logs or other garbage into stdout (shell wrappers are a
//! common offender). Non-header, non-empty lines before the `Content-Length`
//! line are therefore skipped with a debug log instead of tearing down the
//! session. A frame announcing an absurd body size is rejected outright —
//! that is corruption, not logging.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::LspError;

/// Protocol version announced on every outgoing message.
pub const JSONRPC_VERSION: &str = "2.0";

/// Hard ceiling for a single incoming message body (64 MiB).
///
/// Language servers legitimately push large payloads (full-document sync of
/// big files, workspace symbol dumps), but nothing sane approaches this limit.
/// Enforcing it converts a corrupt stream from a potential OOM into a clean
/// error.
pub const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

const HEADER_DELIMITER: &str = "\r\n";
const CONTENT_LENGTH_HEADER: &str = "content-length";
/// Blank line that terminates the header block (spec) — bare `\n` accepted
/// because several servers get line endings wrong.
const BLANK_LINE_ENDINGS: [&[u8]; 2] = [b"\r\n", b"\n"];

/// JSON-RPC / LSP error codes used by the engine.
pub mod error_codes {
    /// Failed to parse a request payload we received from the server.
    pub const PARSE_ERROR: i64 = -32700;
    /// Request we received from the server is not valid JSON-RPC.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Server called a method the client does not implement.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Parameters failed to deserialize.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Generic internal failure while handling a server request.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// LSP: server cancelled a request it had previously accepted.
    pub const SERVER_CANCELLED: i64 = -32802;
    /// LSP: result invalidated before delivery; safe to re-request.
    pub const CONTENT_MODIFIED: i64 = -32801;
    /// LSP: request cancelled via `$/cancelRequest`.
    pub const REQUEST_CANCELLED: i64 = -32800;
}

/// Identifies a JSON-RPC request/response pair.
///
/// Servers are free to use numeric or string ids; both must round-trip exactly
/// (no coercion between `"2"` and `2`). Numeric ids are [`i64`] because some
/// servers emit large numeric ids on their own requests — failing to parse one
/// would leave the server waiting for our response forever.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id (`"id": 2`). Most common in practice.
    Number(i64),
    /// String id (`"id": "anythingAtAll"`). Emitted by some servers.
    String(String),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::String(s) => f.write_str(s),
        }
    }
}

/// JSON-RPC error object carried in responses to our requests and in our
/// responses to server-initiated requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// Machine-readable error code; see [`error_codes`].
    pub code: i64,
    /// Human-readable description.
    pub message: String,
    /// Optional structured detail (e.g. retry hints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Convenience constructor without `data`.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Convert into the engine-wide [`LspError::Rpc`](super::error::LspError).
    pub fn into_lsp_error(self) -> super::error::LspError {
        super::error::LspError::Rpc {
            code: self.code,
            message: self.message,
            data: self.data,
        }
    }
}

/// One message received from the language server.
#[derive(Debug)]
pub enum IncomingMessage {
    /// A server→client request that requires a response.
    Request {
        /// Correlation id echoed back by
        /// [`Transport::respond`](super::Transport::respond).
        id: RequestId,
        /// Fully-qualified method name (e.g. `workspace/configuration`).
        method: Box<str>,
        /// Raw params, present when the server sent them.
        params: Option<Value>,
    },
    /// A fire-and-forget notification (e.g. `publishDiagnostics`).
    Notification {
        /// Fully-qualified method name.
        method: Box<str>,
        /// Raw params, present when the server sent them.
        params: Option<Value>,
    },
    /// A response to one of our requests. Never routed to consumers — the
    /// transport resolves the matching pending entry instead.
    Response {
        /// Id of the originating request.
        id: RequestId,
        /// `Ok` on success, `Err` when the server reported an error object.
        outcome: Result<Value, RpcError>,
    },
}

/// Wire shape of a client→server request.
#[derive(Serialize)]
struct OutgoingRequest<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

/// Wire shape of a client→server notification.
#[derive(Serialize)]
struct OutgoingNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

/// Wire shape of our response to a server→client request.
///
/// Exactly one of `result`/`error` is ever serialized because construction is
/// split across two builders ([`encode_response_ok`] /
/// [`encode_response_err`]) — the "both fields present" class of protocol bug
/// is unrepresentable.
#[derive(Serialize)]
struct OutgoingResponse<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a RpcError>,
}

/// Raw probe of any incoming JSON-RPC message.
///
/// All fields optional so that a single parse pass can classify requests,
/// notifications and responses; classification happens in
/// [`classify_message`].
#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<RequestId>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    // NOTE: `result: null` and a missing `result` both deserialize to `None`.
    // Per zed's precedent (input_handler.rs) a response carrying neither a
    // usable result nor an error is treated as success-with-null rather than
    // rejected — several servers answer `shutdown` with a bare null.
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// Classify a decoded frame body into an [`IncomingMessage`].
///
/// Malformed messages yield `Err`; callers are expected to log-and-skip so a
/// single bad frame cannot kill the session.
pub fn classify_message(body: &str) -> Result<IncomingMessage, LspError> {
    let raw: RawMessage = serde_json::from_str(body)
        .map_err(|err| LspError::MalformedMessage(format!("{err}: {body:.200}")))?;

    if let Some(method) = raw.method {
        return Ok(match raw.id {
            Some(id) => IncomingMessage::Request {
                id,
                method: method.into(),
                params: raw.params,
            },
            None => IncomingMessage::Notification {
                method: method.into(),
                params: raw.params,
            },
        });
    }

    match raw.id {
        Some(id) => {
            let outcome = match raw.error {
                Some(error) => Err(error),
                None => Ok(raw.result.unwrap_or(Value::Null)),
            };
            Ok(IncomingMessage::Response { id, outcome })
        }
        None => Err(LspError::MalformedMessage(format!(
            "message has neither method nor id: {body:.200}"
        ))),
    }
}

/// Serialize a client→server request into a frame body (no headers yet).
pub fn encode_request(id: &RequestId, method: &str, params: Option<&Value>) -> String {
    serde_json::to_string(&OutgoingRequest {
        jsonrpc: JSONRPC_VERSION,
        id,
        method,
        params,
    })
    .expect("request serialization cannot fail")
}

/// Serialize a client→server notification into a frame body.
pub fn encode_notification(method: &str, params: Option<&Value>) -> String {
    serde_json::to_string(&OutgoingNotification {
        jsonrpc: JSONRPC_VERSION,
        method,
        params,
    })
    .expect("notification serialization cannot fail")
}

/// Serialize a successful response to a server→client request.
pub fn encode_response_ok(id: &RequestId, result: &Value) -> String {
    serde_json::to_string(&OutgoingResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: Some(result),
        error: None,
    })
    .expect("response serialization cannot fail")
}

/// Serialize an error response to a server→client request.
pub fn encode_response_err(id: &RequestId, error: &RpcError) -> String {
    serde_json::to_string(&OutgoingResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(error),
    })
    .expect("response serialization cannot fail")
}

/// Wrap a frame body with its `Content-Length` header block.
pub fn encode_frame(body: &str) -> Vec<u8> {
    let mut frame = Vec::with_capacity(
        body.len() + CONTENT_LENGTH_HEADER.len() + HEADER_DELIMITER.len() * 2 + 16,
    );
    frame.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// Read one framed message body from `reader`.
///
/// Buffers are caller-owned and reused across calls to keep allocation flat in
/// the hot loop. Tolerates junk lines before the header block (servers that
/// log into stdout), ignores non-`Content-Length` headers, and rejects frames
/// larger than [`MAX_FRAME_SIZE`] without allocating their announced size.
///
/// Returns the raw JSON body as UTF-8, or [`LspError::StreamClosed`] on EOF.
pub async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    header_buf: &mut Vec<u8>,
    body_buf: &mut Vec<u8>,
) -> Result<String, LspError> {
    let mut seen_length: Option<usize> = None;
    let content_length: usize = loop {
        header_buf.clear();
        let read = reader.read_until(b'\n', header_buf).await?;
        if read == 0 {
            return Err(LspError::StreamClosed);
        }

        let line = std::str::from_utf8(header_buf)
            .map_err(|_| LspError::InvalidFrame("header block is not UTF-8".into()))?
            .trim_end_matches(['\r', '\n']);

        // The blank line terminates the header block — but only counts once a
        // Content-Length was seen; stray blanks before headers (junk from
        // non-conformant servers) are skipped.
        if BLANK_LINE_ENDINGS.contains(&header_buf.as_slice()) {
            match seen_length {
                Some(length) => break length,
                None => continue,
            }
        }

        let Some((name, value)) = line.split_once(':') else {
            log::debug!("skipping non-header line in LSP stream: {line:.100}");
            continue;
        };

        if !name.trim().eq_ignore_ascii_case(CONTENT_LENGTH_HEADER) {
            // Well-formed header we don't care about (e.g. Content-Type).
            continue;
        }

        let length: usize = value.trim().parse().map_err(|_| {
            LspError::InvalidFrame(format!("invalid Content-Length value `{value}`"))
        })?;
        if length > MAX_FRAME_SIZE {
            return Err(LspError::FrameTooLarge {
                size: length,
                max: MAX_FRAME_SIZE,
            });
        }
        if seen_length.replace(length).is_some() {
            // Two different lengths leave framing ambiguous — reject instead
            // of silently picking one (Helix does the same).
            return Err(LspError::InvalidFrame(
                "duplicate Content-Length header".into(),
            ));
        }
    };

    body_buf.resize(content_length, 0);
    reader.read_exact(body_buf).await?;
    // Zero-copy on the happy path; `take` leaves an empty (reused) buffer
    // behind. The error path sacrifices the buffer — corrupt streams end the
    // session anyway.
    String::from_utf8(std::mem::take(body_buf))
        .map_err(|_| LspError::InvalidFrame("frame body is not UTF-8".into()))
}

/// Write one framed message and flush immediately.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, body: &str) -> std::io::Result<()> {
    writer.write_all(&encode_frame(body)).await?;
    writer.flush().await
}
