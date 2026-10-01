//! The decision daemon's server: accept loop, protocol routing, inference
//! serialization, drain.
//!
//! Threading model: the accept loop runs on the main thread; each
//! connection gets a `std::thread`. Inference and model loads are CPU-bound
//! (ORT sessions) and run inline on the connection thread — the
//! `inference_slot` mutex ensures at most one inference at a time, since
//! two would only contend for the same cores. A `decide` that cannot get
//! the slot within [`INFER_QUEUE_WINDOW`] answers [`code::BUSY`], leaving
//! the retry policy to the client.
//!
//! The handshake is enforced per connection: the first message must be
//! `hello` ([`code::HANDSHAKE`] otherwise). `shutdown` flips the server
//! into drain: the accept loop ends, in-flight inferences finish, and the
//! process exits — the next client's activation spawns a fresh daemon.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::daemon::ipc::{self, Message, RpcError};
use crate::daemon::ipc_error::Ipc;

use super::model::Registry;
use super::protocol::{
    self, DecideParams, DecideResult, HealthResult, HelloParams, HelloResult, ShutdownResult, code,
    method,
};

/// How long a `decide` waits for a free inference slot before BUSY.
const INFER_QUEUE_WINDOW: Duration = Duration::from_secs(10);

/// Idle shutdown default: long enough that a working session never bounces
/// the daemon, short enough that an abandoned one does not linger.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The daemon's version, from the crate itself.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Per-connection handler state (the `S` of `ipc::serve_connection`).
struct ConnState {
    handshaken: bool,
}

pub(crate) struct Server {
    registry: Registry,
    /// The one inference slot: a held guard IS the running inference.
    inference_slot: Mutex<()>,
    /// Shared drain flag: once set, the accept loop winds down.
    draining: AtomicBool,
    started: Instant,
    /// Millis since `started` of the last served request — the shared
    /// signal that a daemon with open connections is still useful (the
    /// idle check must not exit under an active connection just because
    /// no new `accept` happened).
    last_request_millis: AtomicU64,
}

impl Server {
    pub(crate) fn new() -> Self {
        Self {
            registry: Registry::new(),
            inference_slot: Mutex::new(()),
            draining: AtomicBool::new(false),
            started: Instant::now(),
            last_request_millis: AtomicU64::new(0),
        }
    }

    /// Serve until the listener errors, the idle budget is exceeded, or a
    /// `shutdown` request drains the server. Returns when the daemon
    /// should exit; `main` then removes the socket file.
    pub(crate) fn serve(
        self: Arc<Self>,
        listener: std::os::unix::net::UnixListener,
        idle_timeout: Duration,
    ) {
        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        let mut last_activity = Instant::now();
        let mut connections = Vec::new();

        loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    last_activity = Instant::now();
                    let server = Arc::clone(&self);
                    connections.push(std::thread::spawn(move || {
                        stream.set_nonblocking(false).ok();
                        // Read timeouts bound every request; a stuck client
                        // or a wedged inference fails the call instead of
                        // pinning the thread forever.
                        stream.set_read_timeout(Some(ipc::CALL_TIMEOUT)).ok();
                        let mut state = ConnState { handshaken: false };
                        if let Err(e) =
                            ipc::serve_connection(stream, &mut state, |state, request| {
                                server.handle(request, &mut state.handshaken)
                            })
                        {
                            log::debug!("decision daemon: connection ended: {e}");
                        }
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No pending connection: idle accounting. The budget
                    // measures the last ACTIVITY — an accept above, or a
                    // request served inside a connection thread (which
                    // stamps `last_request_millis`) — so an open
                    // connection with live traffic never triggers it.
                    if self.idle_for(last_activity) >= idle_timeout {
                        log::info!("decision daemon: idle for {idle_timeout:?}, exiting");
                        break;
                    }
                    // Reap finished connection threads (M2 fix): the
                    // client opens a fresh connection per call, so without
                    // this the handle vec grows by one review per turn.
                    connections.retain(|handle| !handle.is_finished());
                    std::thread::sleep(Duration::from_millis(200));
                }
                // Transient accept failures (EMFILE under fd pressure)
                // must not kill the daemon (m9): back off and retry —
                // clients re-connect; only a permanent listener error
                // (checked below) ends the loop.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    log::warn!("decision daemon: accept failed: {e}");
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    log::warn!("decision daemon: accept failed: {e}");
                    break;
                }
            }

            if self.draining.load(Ordering::SeqCst) {
                break;
            }
        }

        // Drain: wait for in-flight connection threads. A thread inside an
        // inference (or waiting on the slot) does no socket reads, so the
        // CALL_TIMEOUT does NOT bound it — the join here is best-effort
        // drain, not a liveness guarantee (m2). The daemon exits after
        // this regardless; the socket file removal below is the real
        // handoff.
        for handle in connections.drain(..) {
            let _ = handle.join();
        }
    }

    /// Time since the last daemon activity: the later of the last accept
    /// and the last served request.
    fn idle_for(&self, last_accept: Instant) -> Duration {
        let last_request_millis = self
            .last_request_millis
            .load(std::sync::atomic::Ordering::Relaxed);
        if last_request_millis == 0 {
            last_accept.elapsed()
        } else {
            let since_request =
                Instant::now() - self.started - Duration::from_millis(last_request_millis);
            // A request served after the last accept wins; a request before
            // it loses. `elapsed` of the max of the two stamps:
            since_request.min(last_accept.elapsed())
        }
    }

    /// Route one request. `handshaken` is per-connection state: the first
    /// message must be `hello`.
    fn handle(&self, request: Message, handshaken: &mut bool) -> Option<Message> {
        let id = request.request_id()?.clone();
        let Message::Request { method, params, .. } = request else {
            return Some(Message::err(
                id,
                RpcError::new(RpcError::INVALID_REQUEST, "not a request"),
            ));
        };

        if !*handshaken && method != method::HELLO {
            return Some(Message::err(
                id,
                RpcError::new(code::HANDSHAKE, "handshake required: send hello first"),
            ));
        }

        // Stamp activity AFTER the handshake gate (m6): unauthenticated
        // junk (handshake never completed) must not keep the daemon —
        // and its multi-GB resident — alive forever; only a connection
        // that speaks our protocol counts as activity.
        self.last_request_millis.store(
            self.started.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        // Draining: every functional request is refused for the rest of
        // this daemon's life (the client fails open; its next activation
        // spawns a fresh daemon). `shutdown` itself stays routable so a
        // repeated drain is idempotent, and `hello`/`health` still answer.
        if self.draining.load(Ordering::SeqCst)
            && matches!(method.as_str(), m if m == method::DECIDE)
        {
            return Some(Message::err(
                id,
                RpcError::new(code::DRAINING, "daemon is draining for shutdown"),
            ));
        }

        let response = match method.as_str() {
            method::HELLO => self.hello(id, params),
            method::DECIDE => self.decide(id, params),
            method::HEALTH => self.health(id),
            method::SHUTDOWN => self.shutdown(id),
            _ => Message::err(
                id,
                RpcError::new(
                    RpcError::METHOD_NOT_FOUND,
                    format!("unknown method: {method}"),
                ),
            ),
        };
        // The handshake variant must mark the connection as greeted.
        if method == method::HELLO && response.is_ok_shape() {
            *handshaken = true;
        }
        Some(response)
    }

    fn hello(&self, id: Value, params: Option<Value>) -> Message {
        let params: HelloParams = match params.map(ipc::from_value) {
            Some(Ok(params)) => params,
            _ => {
                return Message::err(
                    id,
                    RpcError::new(RpcError::INVALID_PARAMS, "hello params required"),
                );
            }
        };
        if params.protocol > protocol::PROTOCOL {
            return Message::err(
                id,
                RpcError::new(
                    code::HANDSHAKE,
                    format!(
                        "protocol {} newer than daemon {}",
                        params.protocol,
                        protocol::PROTOCOL
                    ),
                ),
            );
        }
        Message::ok(
            id,
            ipc::to_value(&HelloResult {
                protocol: protocol::PROTOCOL,
                daemon: protocol::NAME.to_owned(),
                version: VERSION.to_owned(),
            })
            .unwrap_or(Value::Null),
        )
    }

    /// The one functional method: resident model lookup (loading on first
    /// use of a kind), serialized inference on this connection's thread.
    fn decide(&self, id: Value, params: Option<Value>) -> Message {
        let params: DecideParams = match params.map(ipc::from_value) {
            Some(Ok(params)) => params,
            _ => {
                return Message::err(
                    id,
                    RpcError::new(RpcError::INVALID_PARAMS, "decide params required"),
                );
            }
        };

        let Some(model) = self.registry.resident(&params.kind) else {
            return Message::err(
                id,
                RpcError::new(code::MODEL, "model load failed; retries are throttled"),
            );
        };

        // The single inference slot, with a bounded wait: queued requests
        // poll until the window closes, then BUSY — the client retries once
        // and otherwise fails open.
        let acquired = acquire_slot(&self.inference_slot, INFER_QUEUE_WINDOW);
        let Ok(_guard) = acquired else {
            return Message::err(
                id,
                RpcError::new(code::BUSY, "inference slot not acquired in time"),
            );
        };

        match model.decide(&params.state, &params.questions, None, None, None) {
            Ok(result) => Message::ok(
                id,
                ipc::to_value(&DecideResult { result }).unwrap_or(Value::Null),
            ),
            Err(e) => Message::err(
                id,
                RpcError::new(code::INFERENCE, format!("inference failed: {e}")),
            ),
        }
    }

    fn health(&self, id: Value) -> Message {
        let (requests_served, failed) = self.registry.counters();
        Message::ok(
            id,
            ipc::to_value(&HealthResult {
                uptime_secs: self.started.elapsed().as_secs(),
                active_kind: self.registry.active_kind(),
                requests_served,
                failed,
            })
            .unwrap_or(Value::Null),
        )
    }

    fn shutdown(&self, id: Value) -> Message {
        self.draining.store(true, Ordering::SeqCst);
        Message::ok(
            id,
            ipc::to_value(&ShutdownResult { draining: true }).unwrap_or(Value::Null),
        )
    }
}

impl Message {
    /// Whether this message is a success response (the handshake-completed
    /// signal for the connection state).
    fn is_ok_shape(&self) -> bool {
        matches!(
            self,
            Message::Response {
                result: Some(_),
                error: None,
                ..
            }
        )
    }
}

/// Poll for the inference slot. A std `Mutex` has no timed lock, so the
/// queue is a `try_lock` loop — the poll interval is noise next to the
/// seconds-long inference it is waiting for.
fn acquire_slot(slot: &Mutex<()>, window: Duration) -> Result<std::sync::MutexGuard<'_, ()>, Ipc> {
    let deadline = Instant::now() + window;
    loop {
        match slot.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(Ipc::Timeout);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(Ipc::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "inference slot poisoned",
                )));
            }
        }
    }
}
