//! The daemon transport: UDS + length-prefixed JSON-RPC 2.0, fully
//! synchronous (std only).
//!
//! One frame carries one JSON-RPC message; the frame is a big-endian `u32`
//! length prefix followed by that many bytes of UTF-8 JSON (capped, so
//! garbage cannot force an allocation). JSON-RPC itself is hand-rolled on
//! serde — the envelope is four fields, and owning it keeps the daemon
//! layer dependency-free while the `id` correlation and the standard error
//! object come for free.
//!
//! **Why std, not tokio**: the harness-side consumer (`Checkup`) is a sync
//! trait called from the agent loop's blocking thread; an async client
//! would need a runtime the caller does not have. Daemon traffic is one
//! small request per turn end, so blocking I/O with socket read timeouts
//! is the simpler and correct choice on both ends — the server is
//! thread-per-connection, the client one socket with timeouts.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::ser::Serialize;
use serde_json::Value;

use super::ipc_error::Ipc;

/// Hard ceiling on one request/response round trip, enforced with socket
/// timeouts on the client side (`ClientConnection::new`).
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum frame payload. Real payloads are kilobytes; the cap exists so a
/// corrupted length prefix cannot force a gigabyte allocation before the
/// parser can reject the frame.
const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// Wire envelope. `jsonrpc` is always `"2.0"` and is not modelled as a
/// field the caller can get wrong — [`Message::serialize`] writes it, and
/// parsing rejects anything else. On the wire `result` and `error` are
/// sibling keys, and the spec forbids both or neither.
#[derive(Debug)]
pub(crate) enum Message {
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<RpcError>,
    },
}

impl Message {
    pub(crate) fn request(id: impl Into<Value>, method: &str, params: Value) -> Self {
        Self::Request {
            id: id.into(),
            method: method.to_owned(),
            params: Some(params),
        }
    }

    pub(crate) fn ok(id: Value, result: Value) -> Self {
        Self::Response {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn err(id: Value, error: RpcError) -> Self {
        Self::Response {
            id,
            result: None,
            error: Some(error),
        }
    }

    /// The `id` of a request, for the responder to echo back.
    pub(crate) fn request_id(&self) -> Option<&Value> {
        match self {
            Self::Request { id, .. } => Some(id),
            Self::Response { .. } => None,
        }
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>, Ipc> {
        let (id, method, params, result, error) = match self {
            Self::Request {
                id, method, params, ..
            } => (id, Some(method), params.as_ref(), None, None),
            Self::Response {
                id, result, error, ..
            } => (id, None, None, result.as_ref(), error.as_ref()),
        };
        let mut object = serde_json::map::Map::new();
        object.insert("jsonrpc".into(), Value::from("2.0"));
        object.insert("id".into(), id.clone());
        if let Some(method) = method {
            object.insert("method".into(), Value::from(method.as_str()));
        }
        if let Some(params) = params {
            object.insert("params".into(), params.clone());
        }
        if let Some(result) = result {
            object.insert("result".into(), result.clone());
        }
        if let Some(error) = error {
            object.insert("error".into(), serde_json::to_value(error)?);
        }
        Ok(serde_json::to_vec(&Value::Object(object))?)
    }

    /// Parse one message. Batching (a JSON array) is rejected with
    /// `-32600`: a frame is one message, and a batch adds correlation
    /// complexity no consumer needs.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Ipc> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| Ipc::Parse)?;
        let Value::Object(object) = value else {
            return Err(Ipc::InvalidRequest);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(Ipc::InvalidRequest);
        }
        let id = object
            .get("id")
            .cloned()
            .filter(|id| !id.is_null())
            .ok_or(Ipc::InvalidRequest)?;
        match (
            object.get("method"),
            object.get("result"),
            object.get("error"),
        ) {
            (Some(Value::String(method)), None, None) => Ok(Self::Request {
                id,
                method: method.clone(),
                params: object.get("params").cloned().filter(|p| !p.is_null()),
            }),
            (None, result, error) if result.is_some() ^ error.is_some() => {
                let error = error
                    .cloned()
                    .map(serde_json::from_value::<RpcError>)
                    .transpose()
                    .map_err(|_| Ipc::Parse)?;
                Ok(Self::Response {
                    id,
                    result: result.cloned(),
                    error,
                })
            }
            _ => Err(Ipc::InvalidRequest),
        }
    }
}

/// The JSON-RPC error object. Application codes live in each daemon's
/// protocol; the reserved transport codes live here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub(crate) fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) const PARSE: i32 = -32700;
    pub(crate) const INVALID_REQUEST: i32 = -32600;
    pub(crate) const METHOD_NOT_FOUND: i32 = -32601;
    pub(crate) const INVALID_PARAMS: i32 = -32602;
    pub(crate) const INTERNAL: i32 = -32603;
}

pub(crate) fn write_frame(stream: &mut UnixStream, message: &Message) -> Result<(), Ipc> {
    let payload = message.serialize()?;
    let len = u32::try_from(payload.len()).map_err(|_| Ipc::FrameTooLarge)?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

/// Read one frame; `None` at a clean peer close (EOF between frames).
pub(crate) fn read_frame(stream: &mut UnixStream) -> Result<Option<Vec<u8>>, Ipc> {
    let mut prefix = [0u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(_) => {}
        Err(e) if is_close(&e) => return Ok(None),
        Err(e) if is_timeout(&e) => return Err(Ipc::Timeout),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(prefix);
    if len > MAX_FRAME {
        return Err(Ipc::FrameTooLarge);
    }
    let mut payload = vec![0u8; len as usize];
    match stream.read_exact(&mut payload) {
        Ok(_) => Ok(Some(payload)),
        Err(e) if is_close(&e) => Ok(None),
        Err(e) if is_timeout(&e) => Err(Ipc::Timeout),
        Err(e) => Err(e.into()),
    }
}

/// A peer close mid-frame surfaces as `UnexpectedEof` (or `Reset`).
fn is_close(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
    )
}

/// Socket read timeouts surface as `WouldBlock` (Linux) or `TimedOut`.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Serve one client connection: read requests, hand each to `handler` with
/// the connection's per-connection `state` (the handshake flag lives
/// there), write the response back. Requests are handled **sequentially per
/// connection** — a client that wants concurrency opens another connection
/// (cheap on a UDS).
pub(crate) fn serve_connection<S, F>(
    mut stream: UnixStream,
    state: &mut S,
    mut handler: F,
) -> Result<(), Ipc>
where
    F: FnMut(&mut S, Message) -> Option<Message>,
{
    while let Some(bytes) = read_frame(&mut stream)? {
        let request = match Message::parse(&bytes) {
            Ok(request) => request,
            // A malformed request still deserves the standard error reply —
            // with a null id, since none could be recovered.
            Err(e) => {
                write_frame(
                    &mut stream,
                    &Message::err(Value::Null, RpcError::new(e.rpc_code(), e.to_string())),
                )?;
                continue;
            }
        };
        let Some(response) = handler(state, request) else {
            continue;
        };
        write_frame(&mut stream, &response)?;
    }
    Ok(())
}

/// A client-side connection: the handshake is folded into the first call,
/// then requests serialize over the one socket.
pub(crate) struct ClientConnection {
    stream: UnixStream,
    next_id: u64,
    /// The daemon name this connection expects to greet — a foreign daemon
    /// on our socket path fails the handshake.
    expected_daemon: &'static str,
    expected_protocol: u32,
    handshaken: bool,
}

impl ClientConnection {
    /// Take ownership of the socket and apply the CALL BUDGET (M3):
    /// per-read socket timeouts compose — a frame is a prefix read plus a
    /// payload read, and a retry doubles them — so a wedged daemon could
    /// hold the caller for minutes. The budget caps the connection's
    /// read/write timeouts in one place and bounds the WHOLE call.
    pub(crate) fn with_budget(
        stream: UnixStream,
        expected_daemon: &'static str,
        expected_protocol: u32,
        budget: Duration,
    ) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(budget))?;
        stream.set_write_timeout(Some(budget))?;
        Ok(Self {
            stream,
            next_id: 1,
            expected_daemon,
            expected_protocol,
            handshaken: false,
        })
    }

    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::from(id)
    }

    /// The mandatory first exchange: protocol version + pid in, the
    /// daemon's identity out. A foreign daemon or an incompatible protocol
    /// version fails the connection for good.
    pub(crate) fn handshake(&mut self) -> Result<(), Ipc> {
        let id = self.next_id();
        write_frame(
            &mut self.stream,
            &Message::request(
                id.clone(),
                "hello",
                serde_json::json!({ "protocol": self.expected_protocol, "pid": std::process::id() }),
            ),
        )?;
        let bytes = read_frame(&mut self.stream)?.ok_or(Ipc::Closed)?;
        match Message::parse(&bytes)? {
            Message::Response {
                id: reply,
                result,
                error,
            } if reply == id => {
                if let Some(error) = error {
                    return Err(Ipc::Remote(error));
                }
                let result = result.ok_or(Ipc::InvalidRequest)?;
                let daemon = result
                    .get("daemon")
                    .and_then(Value::as_str)
                    .ok_or(Ipc::InvalidRequest)?;
                let protocol = result
                    .get("protocol")
                    .and_then(Value::as_u64)
                    .ok_or(Ipc::InvalidRequest)?;
                if daemon != self.expected_daemon {
                    return Err(Ipc::Handshake(format!(
                        "expected daemon {:?} on this socket, found {:?}",
                        self.expected_daemon, daemon
                    )));
                }
                // Protocol mismatch in EITHER direction fails the
                // connection for good (m7): a newer daemon answers shapes
                // we do not speak; an OLDER daemon would fail per-call on
                // unknown methods — noisy retries instead of one clean
                // fail-open. The version string is deliberately not
                // compared: a version skew with the same protocol is
                // supported by definition.
                if protocol != self.expected_protocol as u64 {
                    return Err(Ipc::Handshake(format!(
                        "daemon speaks protocol {protocol}, we speak {}",
                        self.expected_protocol
                    )));
                }
                self.handshaken = true;
                Ok(())
            }
            _ => Err(Ipc::InvalidRequest),
        }
    }

    /// Send one request and await its response, matched by `id`.
    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, Ipc> {
        if !self.handshaken {
            self.handshake()?;
        }
        let id = self.next_id();
        write_frame(
            &mut self.stream,
            &Message::request(id.clone(), method, params),
        )?;
        loop {
            let bytes = read_frame(&mut self.stream)?.ok_or(Ipc::Closed)?;
            match Message::parse(&bytes)? {
                Message::Response {
                    id: reply_id,
                    result,
                    error,
                } if reply_id == id => {
                    return match (result, error) {
                        (Some(result), None) => Ok(result),
                        (None, Some(error)) => Err(Ipc::Remote(error)),
                        _ => Err(Ipc::InvalidRequest),
                    };
                }
                // Not ours (a stray notification, an unsolicited frame):
                // keep reading until our response arrives.
                _ => continue,
            }
        }
    }
}

/// Parse a `params`/`result` payload into a typed value.
pub(crate) fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, Ipc> {
    serde_json::from_value(value).map_err(|_| Ipc::InvalidParams)
}

/// Serialize a typed params/result value for the wire.
pub(crate) fn to_value<T: Serialize>(value: &T) -> Result<Value, Ipc> {
    Ok(serde_json::to_value(value)?)
}

/// Bind the listener for `path` — the bind-as-lock.
///
/// The socket file existing does NOT mean the daemon is dead: it may be a
/// live daemon another process activated a moment ago. So, in order:
///
/// 1. file exists AND connect succeeds → a live daemon owns it; fail with
///    `AddrInUse` (the spawning loser must exit, never re-bind);
/// 2. file exists, connect refused → a stale socket of a dead daemon
///    (a crash; a reboot cannot leave one on tmpfs) → remove and bind;
/// 3. no file → bind directly.
///
/// Removing unconditionally would let a spawning loser detach a live
/// daemon's socket from under every connected client.
pub(crate) fn bind(path: &std::path::Path) -> std::io::Result<UnixListener> {
    if path.exists() && UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("a live daemon already owns {}", path.display()),
        ));
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
