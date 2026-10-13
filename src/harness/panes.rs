//! Registry of `/agents` panes and the message bridge between the main
//! agent's harness and those panes (`agents_call`).
//!
//! The registry is PROCESS-WIDE on purpose: the TUI builds a fresh
//! [`Harness`](super::core::Harness) for every turn, while the pane set
//! (and any request already queued to a pane) outlives the turn that
//! addressed it — a per-harness registry would strand every request the
//! moment the loop ended.
//!
//! Ownership split, mirroring `background.rs`:
//!
//! - **The App owns the panes.** It syncs the pane set here whenever the
//!   split changes ([`set_panes`]), serves queued requests when a pane
//!   becomes free (an agent's message to a busy pane WAITS — it is queued,
//!   never interleaved), and reports each pane turn's end back through
//!   [`complete_request`].
//! - **The harness talks.** The `agents_call` tool enqueues a request
//!   ([`enqueue`]) and either awaits the pane's report (`wait` mode) or
//!   returns a receipt immediately (`background` mode — the report is
//!   delivered later by the requester's loop drain, [`take_ready_deliveries`]).
//!
//! Requests are tagged with their REQUESTER (`main` loop or the `manager`
//! loop running in a cosh pane) so a background report is drained only by
//! the loop that asked for it — the two loops never run concurrently (the
//! `/agents` gate refuses to open while the main loop works, and the main
//! submit path routes into the split while it is open), but a delivery can
//! outlive its requester's turn, and the wrong loop must never inject it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// The top-level agent loop (the normal chat session) as a requester tag.
pub const MAIN: &str = "main";
/// The manager loop (the `cosh` pane's own cosh session — the app's own
/// pane, at any split position) as a requester tag.
pub const MANAGER: &str = "manager";

/// One addressable pane of the `/agents` split, as the harness sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDescriptor {
    /// The addressable id: the agent CLI name, or `name#k` when the split
    /// runs several panes of the same CLI (same disambiguation the right
    /// panel's mirrors use). The manager pane is addressable like every
    /// other agent, under `"cosh"` (or `"cosh#k"` among several).
    pub id: String,
    /// The agent CLI backing the pane (`"claude"`, `"kilo"`, …).
    pub agent: String,
    /// Whether this pane runs THIS app's own manager loop (a `cosh` pane).
    /// Manager panes can never free themselves while a peer manager waits on
    /// them, so `agents_call` gates `Wait` calls between them (see
    /// [`enqueue`]); background calls stay allowed.
    pub is_manager: bool,
    /// Whether the pane already carries a bound agent session (`true`) or
    /// has never run (`false` — the first `agents_call` wakes it).
    pub bound: bool,
}

/// How a request's answer reaches the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyMode {
    /// The `agents_call` blocks until the pane's turn finishes and returns
    /// its report as the tool result.
    Wait,
    /// The `agents_call` returns a receipt immediately; the report is
    /// delivered later as an automated notification in the requester's
    /// next loop drain.
    Background,
}

/// One queued message from the agent to a pane.
#[derive(Debug, Clone)]
pub struct PaneRequest {
    /// Registry-unique request id (`pane-<n>`), echoed in receipts and
    /// completion notifications so the App can pair a pane turn with the
    /// request that started it.
    pub id: String,
    /// Addressable pane id the request targets.
    pub pane: String,
    /// The message text (the agent's input to the pane).
    pub text: String,
    /// Which loop asked for this (`MAIN` or `MANAGER`) — routes the
    /// background delivery home.
    pub requester: &'static str,
    /// How the answer reaches the requester.
    pub mode: ReplyMode,
}

/// A finished request whose answer is waiting for its requester's next
/// loop drain (background mode only — wait mode resolves inline).
#[derive(Debug)]
pub struct PaneDelivery {
    /// The pane that produced the answer.
    pub pane: String,
    /// The request this answer completes.
    pub request_id: String,
    /// Which loop drains it (`MAIN` or `MANAGER`).
    pub requester: &'static str,
    /// The pane's final report (or the failure reason).
    pub output: Result<String, String>,
}

/// The wait-mode half of a pending request: the tool's oneshot, resolved
/// by [`complete_request`] when the pane's turn ends.
struct PendingWait {
    tx: tokio::sync::oneshot::Sender<Result<String, String>>,
}

#[derive(Default)]
struct RegistryState {
    panes: Vec<PaneDescriptor>,
    /// One FIFO per pane: requests wait here while the pane is busy (or
    /// until the App's dispatcher picks them up). A pane works on at most
    /// one request at a time — the head of its queue.
    queues: HashMap<String, VecDeque<PaneRequest>>,
    /// Requests the App has popped and dispatched as a pane turn, keyed by
    /// PANE id (the App completes a pane's turn without knowing request
    /// ids — one `complete_request` per `AgentsPaneDone`). One entry per
    /// pane: a pane serves at most one request at a time. The full request
    /// is kept so the completion knows the mode (wait resolves inline;
    /// background parks a delivery) and the requester (routes the delivery).
    serving: HashMap<String, PaneRequest>,
    /// Wait-mode requests whose answer has not arrived yet.
    waits: HashMap<String, PendingWait>,
    /// Monotonic request-id counter.
    next_id: u64,
    /// Finished background deliveries awaiting their requester's drain.
    deliveries: Vec<PaneDelivery>,
}

fn registry() -> &'static Mutex<RegistryState> {
    static REGISTRY: OnceLock<Mutex<RegistryState>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(RegistryState::default()))
}

/// Replace the registry's pane set (the App syncs this on every split
/// change — open, park, attach, replace). Requests queued to panes that
/// no longer exist are failed immediately: their wait-mode callers get an
/// error result and their queued entries are dropped (a pane removed from
/// the split can never serve them).
pub fn set_panes(panes: Vec<PaneDescriptor>) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let removed: Vec<String> = state
        .queues
        .keys()
        .filter(|id| !panes.iter().any(|p| &p.id == *id))
        .cloned()
        .collect();
    for id in removed {
        // Queued requests: failed (the pane can never serve them now).
        if let Some(mut queue) = state.queues.remove(&id) {
            for request in queue.drain(..) {
                fail_wait(&mut state, &request.id, format!("pane '{id}' was removed"));
            }
        }
        // In-flight requests: the pane's turn is being cancelled with the
        // split, so it will never complete — leaving the `serving` entry
        // would keep `has_outstanding_requests` true forever and hang the
        // requester's busy-guard. Fail any unresolved wait with them.
        if let Some(request) = state.serving.remove(&id) {
            fail_wait(&mut state, &request.id, format!("pane '{id}' was removed"));
        }
    }
    // Finished background deliveries of removed panes are ORPHANED: their
    // requester may be long gone, and a stale delivery draining into a
    // LATER loop (a session reopened, a new manager turn) would inject a
    // report from a dead conversation. They die with the pane.
    state
        .deliveries
        .retain(|delivery| panes.iter().any(|p| p.id == delivery.pane));
    state.panes = panes;
}

/// The current pane set (snapshot).
#[must_use]
pub fn list_panes() -> Vec<PaneDescriptor> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .panes
        .clone()
}

/// Whether ANY pane exists. The context block and the `agents_call` tool
/// are both keyed on this: with no panes the context stays byte-identical
/// with the pre-feature header and the tool is not registered at all.
#[must_use]
pub fn has_panes() -> bool {
    !registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .panes
        .is_empty()
}

/// Queue one message to a pane. The request waits in the pane's queue
/// while the pane is busy (a user turn, an earlier agent request) — the
/// App dispatches the head when the pane becomes free. `Wait` mode also
/// returns a receiver the tool awaits for the pane's report; `Background`
/// mode returns `None` (the report arrives via [`take_ready_deliveries`]).
///
/// # Errors
///
/// An unknown pane id (the model guessed a name that is not bound — the
/// error names the known ids so the model can correct itself). Also a
/// `Wait` call from a manager (`MANAGER`) loop to a cosh pane: see the
/// mutual-deadlock guard below.
pub fn enqueue(
    pane: &str,
    text: &str,
    requester: &'static str,
    mode: ReplyMode,
) -> Result<EnqueueHandle, String> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let Some(target) = state.panes.iter().find(|p| p.id == pane) else {
        let known: Vec<&str> = state.panes.iter().map(|p| p.id.as_str()).collect();
        return Err(format!(
            "no pane '{pane}' in this session's /agents split; known panes: {known:?}"
        ));
    };
    // Mutual-deadlock guard: a cosh pane runs a MANAGER loop, and a manager
    // turn cannot end while it waits on another pane. A `Wait` call between
    // cosh panes can therefore deadlock (self on the same id, or two managers
    // on each other's pane: both turns stay busy, `serve_agents_pane_queues`
    // skips running panes, and no turn ever reaches `complete_request`).
    // Background calls are non-blocking and safe — they are the intended
    // coordination path for a planner/implementer pair — so only `Wait` is
    // refused, with a pointer to `run_in_background`. (The separate self-call
    // guard in `Harness` also refuses a background call a pane makes to its
    // OWN id: that request could only run after the current turn, which the
    // busy-guard keeps alive.) A manager may still `Wait` on an ACP pane: an
    // external CLI's turn ends on its own and frees the waiter.
    if requester == MANAGER && target.is_manager && mode == ReplyMode::Wait {
        return Err(format!(
            "`agents_call` to the cosh pane '{pane}' cannot block: cosh panes run manager \
             loops, and a manager turn cannot finish while waiting on a peer pane — two of \
             them waiting on each other would deadlock. Re-run with \
             `run_in_background: true`: the report then arrives as an [automated notification]."
        ));
    }
    state.next_id += 1;
    let request = PaneRequest {
        id: format!("pane-{}", state.next_id),
        pane: pane.to_string(),
        text: text.to_string(),
        requester,
        mode,
    };
    let (tx, rx) = match mode {
        ReplyMode::Wait => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (Some(tx), Some(rx))
        }
        ReplyMode::Background => (None, None),
    };
    if let Some(tx) = tx {
        state.waits.insert(request.id.clone(), PendingWait { tx });
    }
    state
        .queues
        .entry(pane.to_string())
        .or_default()
        .push_back(request);
    Ok(EnqueueHandle {
        request_id: state
            .queues
            .get(pane)
            .and_then(|q| q.back())
            .map(|r| r.id.clone())
            .unwrap_or_default(),
        receiver: rx,
    })
}

/// What [`enqueue`] hands back to the tool.
pub struct EnqueueHandle {
    /// The queued request's id (echoed in receipts so a `subagent_status`-
    /// style poll could be added later without a registry change).
    pub request_id: String,
    /// Wait mode only: resolves with the pane's final report (or the
    /// failure reason — pane removed, turn errored).
    pub receiver: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
}

/// Pop the head of `pane`'s request queue — the request the pane should
/// work on next. Called by the App when the pane is free (split opened
/// with it idle, its previous turn just ended): the App dispatches the
/// returned request as the pane's next turn and reports the result back
/// through [`complete_request`] / [`complete_background`]. `None` when
/// the pane has no queued request.
#[must_use]
pub fn pop_next_request(pane: &str) -> Option<PaneRequest> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let request = state.queues.get_mut(pane)?.pop_front()?;
    state.serving.insert(pane.to_string(), request.clone());
    Some(request)
}

/// Peek whether `pane` has any queued request (the App's "is there work
/// waiting for this pane" check — no dequeue).
#[must_use]
pub fn has_queued_request(pane: &str) -> bool {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .queues
        .get(pane)
        .is_some_and(|q| !q.is_empty())
}

/// Report a pane turn's end to the registry: resolves the request the pane
/// was serving — a `Wait` request unblocks the `agents_call` tool inline
/// with the report, a `Background` request parks a delivery for its
/// requester's next loop drain. Called by the App from the `AgentsPaneDone`
/// handler (keyed by pane — the App never knows request ids). A pane with
/// no tracked request (a pure user turn) is a no-op.
pub fn complete_request(pane: &str, output: Result<String, String>) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let Some(request) = state.serving.remove(pane) else {
        return;
    };
    match request.mode {
        ReplyMode::Wait => {
            if let Some(pending) = state.waits.remove(&request.id) {
                let _ = pending.tx.send(output);
            }
        }
        ReplyMode::Background => state.deliveries.push(PaneDelivery {
            pane: pane.to_string(),
            request_id: request.id,
            requester: request.requester,
            output,
        }),
    }
}

/// Cancel every outstanding request from `requester` whose wait is still
/// unresolved: the pane queue entries are dropped and the waiters resolve
/// with the failure. Called on the requester loop's stop (Esc) — a stopped
/// turn must never leave a wait-mode `agents_call` blocked forever.
pub fn cancel_requester(requester: &'static str) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let stale: Vec<String> = state
        .queues
        .values()
        .flat_map(|queue| queue.iter())
        .filter(|request| request.requester == requester)
        .map(|request| request.id.clone())
        .collect();
    for id in &stale {
        for queue in state.queues.values_mut() {
            queue.retain(|request| &request.id != id);
        }
        fail_wait(
            &mut state,
            id,
            "the requesting turn was stopped".to_string(),
        );
    }
}

/// Resolve ONE wait-mode request with a failure, removing it from the
/// wait map. Called by the tool itself when the requester's turn is
/// stopped while it awaits (Esc): the blocked tool unblocks with the
/// stop error instead of hanging forever. The pane-side bookkeeping is
/// untouched — if the pane was already working on the request, its turn
/// finishes normally and `complete_request` finds no wait to resolve
/// (the report is simply not delivered to a dead turn).
pub fn cancel_wait(request_id: &str) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    // The request may still be QUEUED (never dispatched) or already
    // IN-FLIGHT (the pane is working on it). A queued entry is removed so
    // the App's queue service never dispatches a message whose requester
    // died; an in-flight entry stays — the pane finishes its turn normally
    // and `complete_request` resolves whatever wait is left (none, here).
    for queue in state.queues.values_mut() {
        queue.retain(|request| request.id != request_id);
    }
    fail_wait(
        &mut state,
        request_id,
        "the requesting turn was stopped".to_string(),
    );
}

/// Whether `requester` has any outstanding pane request — queued (waiting
/// for a pane to free) or in-flight (a pane is working on it). The agent
/// loop's busy-guard reads this at its natural-completion point: a turn
/// that asked a pane for something does NOT end while the answer is still
/// outstanding — ending it would orphan the wait and lose the report.
#[must_use]
pub fn has_outstanding_requests(requester: &str) -> bool {
    let state = registry().lock().unwrap_or_else(|p| p.into_inner());
    state
        .queues
        .values()
        .flatten()
        .any(|request| request.requester == requester)
        || state
            .serving
            .values()
            .any(|request| request.requester == requester)
}

/// Fail a wait-mode request (pane removed / registry reset). No-op when
/// the id has no outstanding wait.
fn fail_wait(state: &mut RegistryState, request_id: &str, error: String) {
    if let Some(pending) = state.waits.remove(request_id) {
        let _ = pending.tx.send(Err(error));
    }
}

/// Drain the finished background deliveries tagged for `requester`.
/// Called by the agent loop at its natural-drain point (the same place
/// `background::take_ready_notifications` runs). Undelivered deliveries
/// for the OTHER requester stay parked — the manager loop must never
/// inject a report the main loop asked for, and vice versa.
#[must_use]
pub fn take_ready_deliveries(requester: &str) -> Vec<PaneDelivery> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let mut ready = Vec::new();
    state.deliveries.retain_mut(|delivery| {
        if delivery.requester == requester {
            ready.push(PaneDelivery {
                pane: delivery.pane.clone(),
                request_id: delivery.request_id.clone(),
                requester: delivery.requester,
                output: delivery.output.clone(),
            });
            false
        } else {
            true
        }
    });
    ready
}

/// Render one finished background delivery as the `[automated
/// notification]` user turn both loop drain sites inject (the per-turn
/// drain and the pre-Done busy-guard): same contract, same shape.
pub(crate) fn delivery_note(delivery: &PaneDelivery) -> String {
    let report = match &delivery.output {
        Ok(report) => super::background::sanitize(report),
        Err(error) => error.clone(),
    };
    format!(
        "[automated notification] Pane '{}' (agents_call {}) finished.\nReport:\n{}",
        delivery.pane, delivery.request_id, report
    )
}

/// The context block appended at the END of the system header (an
/// appendix — never a prefix insertion, so the providers' exact-prefix
/// caches stay valid). `None` when no panes exist: the header stays
/// byte-identical with the pre-feature one, and nothing is interpolated
/// for something that does not exist.
#[must_use]
pub fn context_block() -> Option<String> {
    use std::fmt::Write as _;
    let panes = list_panes();
    if panes.is_empty() {
        return None;
    }
    let mut block = String::from(
        "## Agents Panes\n\
         The /agents panes currently bound to this session — address each \
         by its pane id in `agents_call`:\n",
    );
    for pane in &panes {
        let session = if pane.bound {
            "agent session bound"
        } else {
            "no session yet (the first agents_call wakes it)"
        };
        let _ = writeln!(block, "- {} — {} ({})", pane.id, pane.agent, session);
    }
    Some(block)
}

/// The `agents_call` harness tool, built against the CURRENT pane set:
/// the description lists the addressable ids (the model never guesses)
/// and the schema's `pane` enum carries the same ids. Called by the
/// harness builder only when panes exist — with none, the tool is never
/// registered and no context carries it.
#[must_use]
pub fn agents_call_tool() -> super::core::HarnessTool {
    let ids: Vec<String> = list_panes().iter().map(|p| p.id.clone()).collect();
    let pane_list = ids.join(", ");
    let enum_values: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::Value::String(id.clone()))
        .collect();
    super::core::HarnessTool {
        name: "agents_call".into(),
        description: format!(
            "Send a message to one of this session's /agents panes and coordinate its work.\n\
             `pane` is the addressable pane id ({pane_list}); `input` is the message the pane receives as its next turn.\n\
             A pane that is busy (the user's turn, an earlier request) receives your message AFTER its current work — it is queued, never interleaved.\n\
             ## When to use\n\
             - Delegate a task to a pane with everything it needs to act WITHOUT asking you back: the panes are independent agents and do NOT see your conversation.\n\
             - Set `run_in_background: true` to keep working while the pane runs: the call returns a receipt immediately and the pane's report arrives later as an [automated notification]. Prefer this for parallel work.\n\
             - Omit `run_in_background` (default): the call blocks until the pane finishes and its report IS the tool result."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "pane": {
                    "type": "string",
                    "description": "The pane to address. Must be one of the ids listed in the description.",
                    "enum": enum_values,
                },
                "input": {
                    "type": "string",
                    "description": "The message to send to the pane as its next turn.",
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Set true to send WITHOUT blocking: the call returns a receipt and the pane's report arrives later as an [automated notification]. Omitted or false (default): the call blocks until the pane finishes and returns its report directly.",
                },
            },
            "required": ["pane", "input"],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize every test behind one lock (the registry is a
    /// process-global singleton) and RESET it when dropped, so a leaked
    /// pane/request cannot wedge later tests. The guard holds a DEDICATED
    /// test mutex — never the registry's own (the global functions lock
    /// that internally; holding it here would deadlock every call).
    struct RegistryGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for RegistryGuard {
        fn drop(&mut self) {
            *registry().lock().unwrap_or_else(|p| p.into_inner()) = RegistryState::default();
        }
    }
    fn lock() -> RegistryGuard {
        static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
        RegistryGuard(
            SERIAL
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        )
    }

    fn pane(id: &str, agent: &str, bound: bool) -> PaneDescriptor {
        PaneDescriptor {
            id: id.to_string(),
            agent: agent.to_string(),
            // `cosh` is the app's manager label; the production sync sets the
            // flag from `DAEMON_LABEL`, mirrored here for the tests.
            is_manager: agent == "cosh",
            bound,
        }
    }

    #[test]
    fn enqueue_rejects_an_unknown_pane_with_the_known_ids() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        let error = match enqueue("kilo", "hi", MAIN, ReplyMode::Wait) {
            Err(error) => error,
            Ok(_) => panic!("an unknown pane must be rejected"),
        };
        assert!(error.contains("no pane 'kilo'"), "{error}");
        assert!(error.contains("claude"), "{error}");
    }

    /// Drive a wait-mode test to completion on a tiny current-thread
    /// runtime: the tool's half is a plain `await` on the oneshot, so the
    /// test needs an executor (the registry itself is sync).
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    #[test]
    fn wait_mode_resolves_through_complete_request() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        let handle = enqueue("claude", "do the thing", MAIN, ReplyMode::Wait).unwrap();
        let rx = handle.receiver.expect("wait mode carries a receiver");
        // The App pops the head and dispatches it as the pane's turn; the
        // completion is keyed by PANE (the App never knows request ids).
        let request = pop_next_request("claude").expect("queued request");
        assert_eq!(request.id, handle.request_id);
        complete_request("claude", Ok("done".to_string()));
        let answer = block_on(async move { rx.await.expect("sender alive") });
        assert_eq!(answer, Ok("done".to_string()));
    }

    #[test]
    fn background_mode_parks_a_delivery_for_its_requester_only() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", true)]);
        let handle = enqueue("claude", "go", MANAGER, ReplyMode::Background).unwrap();
        assert!(handle.receiver.is_none(), "background carries no receiver");
        // Nothing drained yet, and the MAIN loop must not see the
        // manager's delivery even after completion.
        assert!(take_ready_deliveries(MAIN).is_empty());
        assert!(pop_next_request("claude").is_some(), "dispatched");
        // The completion is keyed by pane; the BACKGROUND mode parks the
        // delivery for the requester's drain.
        complete_request("claude", Ok("report".to_string()));
        let deliveries = take_ready_deliveries(MANAGER);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].pane, "claude");
        assert_eq!(deliveries[0].output.clone().unwrap(), "report".to_string());
        assert!(
            take_ready_deliveries(MAIN).is_empty(),
            "wrong requester never drains"
        );
        // Drained once — a second drain is empty.
        assert!(take_ready_deliveries(MANAGER).is_empty());
    }

    #[test]
    fn replacing_the_pane_set_fails_the_orphaned_waits() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        let handle = enqueue("claude", "hi", MAIN, ReplyMode::Wait).unwrap();
        let rx = handle.receiver.expect("receiver");
        set_panes(vec![pane("kilo", "kilo", false)]);
        let answer = block_on(async move { rx.await.expect("sender alive") });
        assert_eq!(answer, Err("pane 'claude' was removed".to_string()));
    }

    #[test]
    fn cancel_wait_removes_a_still_queued_request() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        let handle = enqueue("claude", "late message", MAIN, ReplyMode::Wait).unwrap();
        let rx = handle.receiver.expect("receiver");
        // The requester's turn stopped BEFORE the App ever dispatched the
        // request: the queued entry must vanish (a later turn must not
        // serve a dead conversation's message) and the wait resolve with
        // the stop failure.
        cancel_wait(&handle.request_id);
        assert!(!has_queued_request("claude"), "queued entry removed");
        assert!(
            !has_outstanding_requests(MAIN),
            "no outstanding requests survive the cancel"
        );
        let answer = block_on(async move { rx.await.expect("sender alive") });
        assert_eq!(answer, Err("the requesting turn was stopped".to_string()));
    }

    #[test]
    fn set_panes_prunes_orphaned_serving_and_deliveries() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", true)]);
        // A background request pops into serving (the App dispatched it),
        // then the pane's turn completes and the delivery parks.
        let handle = enqueue("claude", "go", MANAGER, ReplyMode::Background).unwrap();
        assert!(pop_next_request("claude").is_some());
        complete_request("claude", Ok("report".to_string()));
        // A parked delivery is FINISHED work, not outstanding (it drains on
        // the requester's next turn — the same contract the background
        // sub-agents run on); the drain here is the sanity check.
        assert!(
            !has_outstanding_requests(MANAGER),
            "a parked delivery is finished work, not outstanding"
        );
        assert_eq!(take_ready_deliveries(MANAGER).len(), 1);
        // A SECOND request stays in-flight (serving) when the pane leaves
        // the split: its serving entry is pruned with the pane, and any
        // parked-but-undrained delivery dies with it — a stale report must
        // never drain into a LATER loop.
        enqueue("claude", "second", MAIN, ReplyMode::Background).unwrap();
        assert!(pop_next_request("claude").is_some());
        set_panes(vec![]);
        assert!(
            !has_outstanding_requests(MAIN),
            "orphaned serving pruned with the pane"
        );
        assert!(take_ready_deliveries(MAIN).is_empty());
        assert!(take_ready_deliveries(MANAGER).is_empty());
        assert!(!handle.receiver.is_some(), "background carries no receiver");
    }

    #[test]
    fn context_block_is_none_without_panes_and_lists_them_when_bound() {
        let _g = lock();
        set_panes(vec![]);
        assert!(context_block().is_none(), "no panes → no block, no noise");
        set_panes(vec![
            pane("claude", "claude", true),
            pane("kilo", "kilo", false),
        ]);
        let block = context_block().expect("block");
        assert!(block.starts_with("## Agents Panes"), "{block}");
        assert!(
            block.contains("- claude — claude (agent session bound)"),
            "{block}"
        );
        assert!(block.contains("- kilo — kilo (no session yet"), "{block}");
    }

    /// A manager `Wait` call to another cosh pane is refused (mutual-deadlock
    /// guard) and points at background mode; the same call in background is
    /// accepted — the intended planner/implementer coordination path.
    #[test]
    fn manager_wait_to_a_cosh_pane_is_refused_but_background_is_allowed() {
        let _g = lock();
        set_panes(vec![
            pane("cosh#1", "cosh", false),
            pane("cosh#2", "cosh", false),
        ]);
        let err = match enqueue("cosh#2", "plan", MANAGER, ReplyMode::Wait) {
            Err(error) => error,
            Ok(_) => panic!("cosh→cosh wait must be refused"),
        };
        assert!(err.contains("cannot block"), "{err}");
        assert!(err.contains("run_in_background"), "suggests the fix: {err}");

        let handle = enqueue("cosh#2", "plan", MANAGER, ReplyMode::Background)
            .expect("background cosh→cosh is the supported path");
        assert!(handle.receiver.is_none(), "background carries no receiver");
        assert!(pop_next_request("cosh#2").is_some(), "queued and servable");

        // The reverse direction must behave identically: the guard is about
        // the cosh↔cosh pair, not the specific ids or ordering.
        let reverse = match enqueue("cosh#1", "plan", MANAGER, ReplyMode::Wait) {
            Err(error) => error,
            Ok(_) => panic!("cosh#2→cosh#1 wait must be refused too"),
        };
        assert!(reverse.contains("cannot block"), "{reverse}");
        assert!(
            enqueue("cosh#1", "plan", MANAGER, ReplyMode::Background).is_ok(),
            "background cosh#2→cosh#1 is the supported path"
        );
    }

    /// The guard is about cosh→cosh only: a manager may still `Wait` on an ACP
    /// pane (its turn ends on its own), and the MAIN loop may `Wait` on a cosh
    /// pane (the main loop is not a pane and cannot be the one blocking it).
    #[test]
    fn manager_wait_to_an_acp_pane_and_main_wait_to_cosh_stay_allowed() {
        let _g = lock();
        set_panes(vec![
            pane("claude", "claude", false),
            pane("cosh", "cosh", false),
        ]);
        assert!(
            enqueue("claude", "review", MANAGER, ReplyMode::Wait).is_ok(),
            "manager → ACP wait is safe"
        );
        assert!(
            enqueue("cosh", "coordinate", MAIN, ReplyMode::Wait).is_ok(),
            "main → cosh wait is safe"
        );
    }
}
