//! Tokio transport carrying JSON-RPC messages to and from a language server.
//!
//! Owns three background tasks per server process: a reader demultiplexing
//! stdout frames, a single writer serializing access to stdin (preserving FIFO
//! order of document synchronization messages), and an optional stderr drainer.
//!
//! # Design notes (behavioral port from Zed/Helix)
//!
//! * **Bounded incoming queue** ([`INCOMING_CAPACITY`]): when the consumer
//!   falls behind, the reader stops draining stdout and OS-pipe backpressure
//!   throttles the server instead of growing our heap without limit. The
//!   outbound queue is bounded too ([`OUTBOUND_CAPACITY`]) so a wedged server
//!   surfaces as [`LspError::Backpressure`] instead of unbounded buffering.
//! * **Every request carries a deadline** starting at creation: the returned
//!   [`RequestFuture`] resolves to [`LspError::Timeout`], forgets its pending
//!   entry and emits `$/cancelRequest`; a late response is discarded instead
//!   of surfacing stale data.
//! * **Cancel-on-drop**: dropping an unresolved [`RequestFuture`] does the
//!   same cleanup — even if it was never polled (futures are lazy, so the
//!   cancellation guard cannot live inside the future body). Any *delivered*
//!   response, success or server-reported error, closes the transaction and
//!   disarms the cancellation.
//! * **Session lifecycle**: terminal conditions (stdout EOF, stdin write
//!   failure, oversized frame) flip the [`Transport::exited`] watch, fail
//!   every in-flight request with [`LspError::StreamClosed`] and make
//!   subsequent sends return [`LspError::NotRunning`]. Dropping the handle
//!   ends the session too. Nothing panics; the session degrades cleanly.
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
};

use super::{
    error::{ExitReason, LspError},
    jsonrpc::{self, IncomingMessage, RequestId, RpcError},
};

/// Method used to tell the server a request is no longer wanted.
const CANCEL_REQUEST_METHOD: &str = "$/cancelRequest";

/// Upper bound on messages buffered between the stdout reader and the
/// consumer. Once full, the reader blocks, letting the OS pipe apply
/// backpressure to the server instead of buffering unbounded memory.
///
/// Mirrors Zed's `INCOMING_MESSAGE_QUEUE_CAPACITY`.
pub(crate) const INCOMING_CAPACITY: usize = 128;

/// Upper bound on outbound frames queued for the writer task. Generous for
/// agent-driven traffic; hitting it means the server stopped reading stdin,
/// which is a failure worth surfacing instead of buffering forever.
const OUTBOUND_CAPACITY: usize = 256;

/// Lines retained from stderr for error reporting (init failures surface the
/// server's own crash output).
const STDERR_TAIL_LINES: usize = 100;

enum Outbound {
    /// A serialized frame body awaiting `Content-Length` wrapping.
    Frame(String),
    /// Drain the writer queue down to this point and signal completion.
    Flush(oneshot::Sender<()>),
    /// Final sentinel: flush and exit the writer task.
    Close,
}

struct PendingEntry {
    tx: oneshot::Sender<Result<Value, LspError>>,
}

/// State shared between the transport handle and its background tasks.
struct Shared {
    name: String,
    next_id: AtomicI64,
    pending: Mutex<HashMap<RequestId, PendingEntry>>,
    outbound_tx: mpsc::Sender<Outbound>,
    exit_tx: watch::Sender<Option<ExitReason>>,
}

impl Shared {
    /// Record the terminal condition and fail every in-flight request.
    ///
    /// First caller wins: later calls are no-ops so the reason observed by
    /// [`Transport::exited`] subscribers stays stable once set. (The check
    /// below is racy in theory; every racer records *a* terminal condition,
    /// which is all consumers rely on.)
    fn terminate(&self, reason: ExitReason) {
        if self.exit_tx.borrow().is_some() {
            return;
        }
        let _ = self.exit_tx.send(Some(reason));
        let mut pending = self.pending.lock().expect("pending map lock");
        for (_, entry) in pending.drain() {
            let _ = entry.tx.send(Err(LspError::StreamClosed));
        }
    }
}

/// Handle to a running language-server connection.
///
/// Cheap to share behind an `Arc` internally; the type itself is `!Clone` and
/// owns the receiving end of the incoming message queue.
pub struct Transport {
    shared: Arc<Shared>,
    /// Wrapped in an async mutex so `recv` can be reached through `&self`.
    /// Single-consumer semantics are preserved: each call takes the next
    /// message. `None` after [`Self::take_incoming_rx`] handed the queue to
    /// an exclusive owner (the client layer's dispatcher task).
    incoming_rx: tokio::sync::Mutex<Option<mpsc::Receiver<IncomingMessage>>>,
    stderr_tail: Option<Arc<Mutex<VecDeque<String>>>>,
}

impl Transport {
    /// Start the reader/writer/stderr tasks over the given streams.
    ///
    /// Generic over the I/O types so tests can drive the transport over
    /// in-memory duplex pipes instead of real processes.
    ///
    /// The tasks live exactly as long as the returned handle: dropping it
    /// terminates them (see [`Drop for Transport`]). Killing the server
    /// *process* stays the caller's responsibility — the transport only owns
    /// the byte streams.
    pub fn start<I, O, E>(name: &str, input: I, output: O, stderr: Option<E>) -> Self
    where
        I: AsyncRead + Unpin + Send + 'static,
        O: AsyncWrite + Unpin + Send + 'static,
        E: AsyncRead + Unpin + Send + 'static,
    {
        // Bounded so a wedged server (one that stops reading stdin) cannot
        // grow our memory without limit; agent-driven request volume is far
        // below this capacity in normal operation.
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_CAPACITY);
        let (exit_tx, _exit_rx) = watch::channel(None);

        let shared = Arc::new(Shared {
            name: name.to_owned(),
            next_id: AtomicI64::new(0),
            pending: Mutex::new(HashMap::new()),
            outbound_tx,
            exit_tx,
        });

        // Reader: stdout frames → pending-map routing / incoming queue.
        {
            let shared = Arc::clone(&shared);
            let reader = BufReader::new(input);
            tokio::spawn(read_loop(shared, reader, incoming_tx));
        }

        // Writer: sole owner of stdin.
        {
            let shared = Arc::clone(&shared);
            tokio::spawn(write_loop(shared, output, outbound_rx));
        }

        // Stderr: drained so the pipe never fills and deadlocks the server.
        let stderr_tail = stderr.map(|stderr| {
            let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
            tokio::spawn(stderr_loop(
                Arc::clone(&shared),
                Arc::clone(&tail),
                BufReader::new(stderr),
            ));
            tail
        });

        log::info!("started LSP transport for `{}`", shared.name);
        Self {
            shared,
            incoming_rx: tokio::sync::Mutex::new(Some(incoming_rx)),
            stderr_tail,
        }
    }

    /// Hand the incoming queue to an exclusive owner.
    ///
    /// The client layer's dispatcher task owns the receiver outright — that
    /// lets it await messages without pinning the client's `Arc` alive
    /// (holding it across `recv` would make owner-drop wait on the task,
    /// which waits on owner-drop: a deadlock).
    pub(crate) fn take_incoming_rx(&mut self) -> mpsc::Receiver<IncomingMessage> {
        self.incoming_rx
            .get_mut()
            .take()
            .expect("incoming queue taken twice")
    }

    /// Server name given at construction (used in logs).
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Allocate the next request id (monotonic, process-wide per server).
    pub(crate) fn next_id(&self) -> RequestId {
        RequestId::Number(self.shared.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Receive the next server-initiated request or notification.
    ///
    /// Returns `None` after the reader task terminated (stream death was
    /// already reported through [`Self::exited`]).
    ///
    /// Cancel-safe: dropping the future before the message is consumed leaves
    /// it queued for the next call.
    pub async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.as_mut()?.recv().await
    }

    /// Try to receive without waiting (used by tests and polling callers).
    pub fn try_recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.try_lock().ok()?.as_mut()?.try_recv().ok()
    }

    /// Send a notification; failures mean the session already ended.
    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), LspError> {
        self.enqueue_frame(jsonrpc::encode_notification(method, params.as_ref()))
    }

    /// Respond to a server-initiated request previously delivered through
    /// [`Self::recv`].
    pub fn respond(
        &self,
        id: &RequestId,
        outcome: Result<&Value, &RpcError>,
    ) -> Result<(), LspError> {
        let body = match outcome {
            Ok(result) => jsonrpc::encode_response_ok(id, result),
            Err(error) => jsonrpc::encode_response_err(id, error),
        };
        self.enqueue_frame(body)
    }

    /// Issue a request and await its response (or timeout).
    ///
    /// Lifecycle guarantees:
    /// * **Timeout**: resolves to [`LspError::Timeout`], forgets the pending
    ///   entry and emits `$/cancelRequest` immediately — a late response is
    ///   discarded instead of surfacing stale data.
    /// * **Cancel-on-drop**: dropping this future before a successful
    ///   response does the same cleanup, even if it was never polled
    ///   (futures are lazy, so the guard cannot live inside the body).
    pub fn request(&self, method: &str, params: Option<Value>, timeout: Duration) -> RequestFuture {
        let id = self.next_id();

        let (tx, rx) = oneshot::channel();
        let session_dead = {
            let mut pending = self.shared.pending.lock().expect("pending map lock");
            pending.insert(id.clone(), PendingEntry { tx });
            // Re-checked while holding the pending lock: `terminate` drains
            // this same map AFTER flipping the terminal watch, so either our
            // entry was drained (its sender got `StreamClosed`) or we observe
            // the terminal state here and undo the insert ourselves. Either
            // way a request against a dead session never waits out its
            // deadline on a response that will never come.
            self.shared.exit_tx.borrow().is_some()
        };
        if session_dead {
            self.shared
                .pending
                .lock()
                .expect("pending map lock")
                .remove(&id);
            return RequestFuture {
                inner: Box::pin(async { Err(LspError::NotRunning) }),
            };
        }

        if self
            .enqueue_frame(jsonrpc::encode_request(&id, method, params.as_ref()))
            .is_err()
        {
            self.shared
                .pending
                .lock()
                .expect("pending map lock")
                .remove(&id);
            return RequestFuture {
                inner: Box::pin(async { Err(LspError::NotRunning) }),
            };
        }

        let method_name: Box<str> = method.into();
        // Built eagerly, OUTSIDE the (lazy) future: a future dropped before its
        // first poll must still clean up its pending entry and cancel the
        // request. The `let .. = guard` re-bind inside the block forces the
        // whole struct to move in — field-precise capture would leave the
        // guard outside where its Drop fires immediately.
        //
        // The deadline also starts at creation (not first poll) so buffering
        // the future somewhere cannot stretch it.
        let deadline = tokio::time::sleep(timeout);
        let guard = CancelGuard {
            shared: Arc::clone(&self.shared),
            id: id.clone(),
            armed: true,
        };

        RequestFuture {
            inner: Box::pin(async move {
                let mut cancel_guard = guard;
                tokio::pin!(deadline);

                let outcome = tokio::select! {
                    response = rx => {
                        // Any delivered response — success OR server-reported
                        // error — closes the transaction. Only an unanswered
                        // (timed-out / dropped / dead-session) request cancels.
                        cancel_guard.armed = false;
                        response.unwrap_or(Err(LspError::StreamClosed))
                    }
                    _ = &mut deadline => {
                        Err(LspError::Timeout { method: method_name })
                        // Falling out of the block drops the armed guard,
                        // which forgets the entry and cancels server-side.
                    }
                };

                outcome
            }),
        }
    }

    /// Wait until every frame enqueued so far reached the OS pipe. Used by the
    /// shutdown sequence (`exit` notification must land before the process is
    /// killed).
    pub async fn flush(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self
            .shared
            .outbound_tx
            .try_send(Outbound::Flush(done_tx))
            .is_err()
        {
            // Writer gone or queue wedged: nothing left to await.
            return;
        }
        let _ = done_rx.await;
    }

    /// Watch channel flipping to `Some(reason)` exactly once when the
    /// transport terminates.
    pub fn exited(&self) -> watch::Receiver<Option<ExitReason>> {
        self.shared.exit_tx.subscribe()
    }

    /// Last [`STDERR_TAIL_LINES`] stderr lines joined with newlines, for
    /// surfacing a server's crash output.
    pub fn stderr_tail(&self) -> Option<String> {
        self.stderr_tail.as_ref().map(|tail| {
            tail.lock()
                .expect("stderr tail lock")
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// Best-effort graceful teardown of the writer task (final flush, then
    /// stop). Also terminates the session: subsequent sends fail with
    /// [`LspError::NotRunning`] deterministically, and the terminal watch
    /// flips for [`Self::exited`] subscribers. Forceful termination of the
    /// process remains the owner's job.
    pub fn close(&self) {
        let _ = self.shared.outbound_tx.try_send(Outbound::Close);
        self.shared.terminate(ExitReason::StreamClosed);
    }

    fn enqueue_frame(&self, body: String) -> Result<(), LspError> {
        match self.shared.outbound_tx.try_send(Outbound::Frame(body)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(LspError::Backpressure),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(LspError::NotRunning),
        }
    }
}

/// Ends the session when the handle is dropped: the reader and stderr tasks
/// observe the terminal flip through their private watch receivers and exit
/// immediately (even mid-read), while the writer flushes whatever is queued
/// and stops. In-flight requests are failed with [`LspError::StreamClosed`].
///
/// Killing the server *process* remains the owner's job — the transport owns
/// only the byte streams.
impl Drop for Transport {
    fn drop(&mut self) {
        self.shared.terminate(ExitReason::StreamClosed);
        let _ = self.shared.outbound_tx.try_send(Outbound::Close);
    }
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport")
            .field("name", &self.shared.name)
            .finish_non_exhaustive()
    }
}

/// Future returned by [`Transport::request`].
///
/// Dropping it before a successful response emits `$/cancelRequest` for the
/// underlying request (see [`Transport::request`]).
#[must_use = "dropping the future cancels the request"]
pub struct RequestFuture {
    inner: Pin<Box<dyn Future<Output = Result<Value, LspError>> + Send>>,
}

impl Future for RequestFuture {
    type Output = Result<Value, LspError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

/// Cancels a request when dropped while still armed: forgets the pending
/// entry (so late responses are discarded instead of hitting a dangling
/// receiver) and emits `$/cancelRequest` on the wire.
struct CancelGuard {
    shared: Arc<Shared>,
    id: RequestId,
    armed: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.shared
            .pending
            .lock()
            .expect("pending map lock")
            .remove(&self.id);
        let body =
            jsonrpc::encode_notification(CANCEL_REQUEST_METHOD, Some(&json!({ "id": self.id })));
        // Best-effort: after session death there is nobody to cancel.
        let _ = self.shared.outbound_tx.try_send(Outbound::Frame(body));
    }
}

/// Route a decoded response to whoever is waiting for it, if anyone.
fn route_response(shared: &Shared, id: RequestId, outcome: Result<Value, RpcError>) {
    let outcome = outcome.map_err(RpcError::into_lsp_error);
    let entry = shared.pending.lock().expect("pending map lock").remove(&id);
    match entry {
        Some(entry) => {
            let _ = entry.tx.send(outcome);
        }
        None => {
            // Late answer to an already-timed-out request, or a server bug.
            log::debug!(
                "{} discarding response without matching request (id={id})",
                shared.name
            );
        }
    }
}

async fn read_loop<I>(
    shared: Arc<Shared>,
    mut reader: BufReader<I>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
) where
    I: AsyncRead + Unpin + Send,
{
    let mut header_buf = Vec::with_capacity(128);
    let mut body_buf = Vec::new();
    // Private view of the terminal state: flipping it (including from
    // `Transport::drop`) aborts a blocked frame read immediately.
    let mut exit_rx = shared.exit_tx.subscribe();

    loop {
        let frame = tokio::select! {
            biased;
            _ = exit_rx.changed() => break,
            frame = jsonrpc::read_frame(&mut reader, &mut header_buf, &mut body_buf) => frame,
        };

        match frame {
            Ok(body) => {
                log::trace!("{} <- {body}", shared.name);
                match jsonrpc::classify_message(&body) {
                    Ok(IncomingMessage::Response { id, outcome }) => {
                        route_response(&shared, id, outcome);
                    }
                    Ok(message) => {
                        // Blocks when the consumer falls behind: the OS pipe
                        // then applies backpressure to the server.
                        if incoming_tx.send(message).await.is_err() {
                            // Consumer gone (transport dropped) — nothing left
                            // to serve.
                            break;
                        }
                    }
                    Err(err) => {
                        log::warn!("{} skipping malformed message: {err}", shared.name);
                    }
                }
                // Yield between messages so a notification flood cannot starve
                // other tasks on the runtime (Zed does the same).
                tokio::task::yield_now().await;
            }
            Err(LspError::StreamClosed) => {
                log::info!("{} stdout closed", shared.name);
                shared.terminate(ExitReason::StreamClosed);
                break;
            }
            Err(err) => {
                // Framing or I/O corruption: the byte stream can no longer be
                // trusted, so the session ends here.
                log::warn!(
                    "{} terminating on unrecoverable read error: {err}",
                    shared.name
                );
                shared.terminate(ExitReason::Protocol(err.to_string()));
                break;
            }
        }
    }
}

async fn write_loop<O>(
    shared: Arc<Shared>,
    mut output: O,
    mut outbound_rx: mpsc::Receiver<Outbound>,
) where
    O: AsyncWrite + Unpin + Send,
{
    while let Some(outbound) = outbound_rx.recv().await {
        match outbound {
            Outbound::Frame(body) => {
                log::trace!("{} -> {body}", shared.name);
                if let Err(err) = jsonrpc::write_frame(&mut output, &body).await {
                    log::warn!("{} stdin write failed: {err}", shared.name);
                    shared.terminate(ExitReason::WriteFailed(err.to_string()));
                    break;
                }
            }
            Outbound::Flush(done) => {
                let flushed = output.flush().await;
                let _ = done.send(());
                if let Err(err) = flushed {
                    log::warn!("{} stdin flush failed: {err}", shared.name);
                    shared.terminate(ExitReason::WriteFailed(err.to_string()));
                    break;
                }
            }
            Outbound::Close => {
                let _ = output.flush().await;
                break;
            }
        }
    }
}

async fn stderr_loop<E>(
    shared: Arc<Shared>,
    tail: Arc<Mutex<VecDeque<String>>>,
    mut stderr: BufReader<E>,
) where
    E: AsyncRead + Unpin + Send,
{
    let mut exit_rx = shared.exit_tx.subscribe();
    let mut line = String::new();
    loop {
        let read = tokio::select! {
            biased;
            _ = exit_rx.changed() => break,
            read = stderr.read_line(&mut line) => read,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                // Stored pre-trimmed so `stderr_tail` can join with newlines.
                let mut text = std::mem::take(&mut line);
                let trimmed = text.trim_end().to_owned();
                text.clear();
                line = text;
                log::debug!("lsp stderr: {trimmed}");
                let mut tail = tail.lock().expect("stderr tail lock");
                if tail.len() >= STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(trimmed);
            }
        }
    }
}
