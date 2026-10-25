//! Registry of `/agents` panes and the two bridges between this app's agent
//! loops and those panes.
//!
//! - [`enqueue`] / [`complete_request`] / [`take_ready_deliveries`] carry
//!   `agents_call` traffic: a message to a pane, answered by the pane's turn
//!   (wait mode resolves inline, background mode parks a [`PaneDelivery`]).
//! - [`enqueue_worktree_op`] / [`peek_worktree_op`] / [`pop_worktree_op`] /
//!   [`complete_worktree_op`] carry `agents_worktree` asks: the tool parks
//!   an op, the App drains it (it owns the permission box and every git
//!   side effect) and the outcome normally flows back INTO the paused tool
//!   call as its result ([`take_ready_worktree_delivery`]); only a
//!   stop-interrupted ask falls back to a [`DeliveryKind::Worktree`]
//!   delivery on a later turn.
//!
//! Both bridges share ONE contract, which is why they share this module and
//! the same pane ids: a tool call never blocks ON GIT or on anything the
//! registry cannot answer — the MAIN loop's `agents_worktree` tool is the
//! ONE exception, and it pauses cleanly inside `dispatch_next` while the
//! App owns the permission box and the git side effects. Everything else
//! parks its ask, returns a receipt, and the truth arrives later through the
//! requester's loop drain ([`take_ready_deliveries`] →
//! [`delivery_note`]). Nothing here is `agents_call`-specific.
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
    /// The worktree (git branch checkout) the pane is currently pinned to,
    /// by name — `None` while the pane shares the session's cwd. The main
    /// agent's pane listing renders it next to the id so it always knows
    /// where each pane works.
    pub worktree: Option<String>,
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

/// What a parked delivery carries — an `agents_call` pane report or a
/// finished `agents_worktree` op. The drain and the requester tagging are
/// shared; the note format differs per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryKind {
    /// A pane turn's final report (background `agents_call`).
    CallReport,
    /// A worktree op's outcome (executed, refused by the user, or failed).
    Worktree,
}

/// A finished request whose answer is waiting for its requester's next
/// loop drain, or a finished worktree op's closing note (background mode
/// only — wait mode resolves inline; worktree ops normally do not linger:
/// the tool call pauses inside `dispatch_next` and consumes the outcome
/// itself, so a [`DeliveryKind::Worktree`] delivery here is the
/// EXCEPTION path — an ask that was stop-interrupted or otherwise left
/// parked reaches its loop on a later turn through this same drain).
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
    /// Report or worktree outcome — selects the delivery note's shape.
    pub kind: DeliveryKind,
}

/// What an `agents_worktree` ask does to its target pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeAction {
    /// Create a temp worktree from the pane's current HEAD and pin the
    /// pane to it.
    Assign,
    /// Drop the pin — the pane returns to the session's cwd. Nothing on
    /// disk is removed (the same semantics as the user's `/worktree exit`).
    Clear,
}

impl WorktreeAction {
    /// The wire name the tool's schema speaks (`action`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assign => "assign",
            Self::Clear => "clear",
        }
    }
}

/// One parked `agents_worktree` call — the main agent asking to move a
/// pane into (or out of) a worktree. The TOOL never touches git: it
/// records the ask here, and the App's frame drain pops, supervises
/// (the user's supremacy over a User-assigned worktree — even in Yolo)
/// and executes it, owning every side effect.
#[derive(Debug, Clone)]
pub struct WorktreeOp {
    /// Registry-unique op id (`wt-op-<n>`), echoed in the tool result and in
    /// any later Worktree delivery so the requesting loop can pair them.
    pub id: String,
    /// Addressable pane id the op targets — the same ids `agents_call`
    /// addresses (`agents_worktree_tool` carries them as its schema enum).
    pub pane: String,
    /// Assign or clear.
    pub action: WorktreeAction,
    /// The branch name for `Assign` (its slug names the temp directory).
    /// `None` on `Clear`.
    pub branch: Option<String>,
    /// Which loop parked the op — routes the outcome delivery home. Always
    /// [`MAIN`] in practice: `agents_worktree` is stripped from every pane
    /// loop (`with_own_pane_id`) and the dispatch arm refuses a
    /// pane-originated call before the registry is touched, so the manager
    /// loop can never park one. The tag is kept (rather than hardcoded at
    /// the delivery site) because it is the requester routing every other
    /// delivery already uses; if a pane ever gains this power it stays
    /// correct instead of silently routing to `main`.
    pub requester: &'static str,
}

/// The wait-mode half of a pending request: the tool's oneshot, resolved
/// by [`complete_request`] when the pane's turn ends.
struct PendingWait {
    tx: tokio::sync::oneshot::Sender<Result<String, String>>,
}

#[derive(Default)]
struct RegistryState {
    panes: Vec<PaneDescriptor>,
    /// Monotonic generation counter, bumped by [`set_panes`] whenever the
    /// pane set changes (ids, bindings or worktrees). Panes that ride their
    /// awareness as a prompt preamble (the ACP panes — ACP has no system
    /// prompt) compare their last-announced generation against it to know
    /// when the announcement went stale and must be re-sent.
    generation: u64,
    /// Per-pane id, the generation whose pane set the pane was last told
    /// about (see [`take_preamble`]). Only ACP panes use this.
    announced: HashMap<String, u64>,
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
    /// Parked `agents_worktree` calls (FIFO across all panes): the tool
    /// parks its request; the App's frame drain pops, supervises (the
    /// user's supremacy over a User-assigned worktree — even in Yolo) and
    /// executes it, owning every side effect.
    ops: VecDeque<WorktreeOp>,
    /// Monotonic worktree-op id counter (`wt-op-<n>`).
    next_op: u64,
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
    // report from a dead conversation. They die with the pane. Worktree
    // outcomes are the exception: their RECEIPT-holder (the requesting
    // loop) is alive and waiting for the answer, so a worktree finding —
    // including a failure committed after the pane closed — survives.
    state.deliveries.retain(|delivery| {
        delivery.kind == DeliveryKind::Worktree || panes.iter().any(|p| p.id == delivery.pane)
    });
    // Parked worktree ops of removed panes die immediately: the pane can
    // never be rebound again. complete them as failures so the requesting
    // loop's outcome delivery still lands (themselves Worktree-kind
    // deliveries, so the retain above just kept the door open for them).
    let op_targets: Vec<(String, &'static str)> = state
        .ops
        .iter()
        .filter(|op| !panes.iter().any(|p| p.id == op.pane))
        .map(|op| (op.id.clone(), op.requester))
        .collect();
    for (op_id, requester) in &op_targets {
        let pane = state
            .ops
            .iter()
            .find(|op| &op.id == op_id)
            .map(|op| op.pane.clone())
            .unwrap_or_default();
        state.deliveries.push(PaneDelivery {
            pane: pane.clone(),
            request_id: op_id.clone(),
            requester,
            output: Err(format!(
                "pane '{pane}' was removed — the binding cannot change"
            )),
            kind: DeliveryKind::Worktree,
        });
    }
    state.ops.retain(|op| panes.iter().any(|p| p.id == op.pane));
    // The announced-generation map only tracks panes that still exist; a
    // re-added pane starts unannounced (generation 0 vs the live counter)
    // so its next dispatch re-announces from scratch.
    state
        .announced
        .retain(|id, _| panes.iter().any(|p| p.id == *id));
    state.panes = panes;
    state.generation += 1;
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
    // A leftover serving entry on a pane the App is about to dispatch to
    // is ORPHANED: the App only pops when the pane is free (`!running`),
    // so any entry still here belongs to a turn that never completed —
    // its dispatch was refused after the pop (manager-session mint
    // failure), or its terminal `AgentsPaneDone` was dropped against a
    // torn-down epoch. Leaving it would keep `has_outstanding_requests`
    // true forever and hang the requester's busy-guard (the manager loop
    // waits at its natural-completion point) — the pane would sit
    // `running` with no activity behind it. Fail the stale wait so the
    // requester unblocks instead.
    if let Some(request) = state.serving.remove(pane) {
        fail_wait(
            &mut state,
            &request.id,
            format!("pane '{pane}' orphaned the request: its turn never completed"),
        );
    }
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
///
/// Returns the resolved request's requester tag (`Some(MAIN)` /
/// `Some(MANAGER)`) — the App's wake promise reads it: a MAIN-requested
/// pane turn that completes while the main loop is NOT running must wake
/// the main agent with the report (the caller promised an output for its
/// input; a user-initiated pane turn promises nothing). `None` when the
/// pane served nothing tracked.
#[must_use]
pub fn complete_request(pane: &str, output: Result<String, String>) -> Option<&'static str> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let request = state.serving.remove(pane)?;
    let requester = request.requester;
    match request.mode {
        ReplyMode::Wait => {
            // A LIVE waiter takes the report inline. A DEAD one (the
            // requester's turn ended or was stopped while the pane worked
            // — the oneshot was dropped) must not swallow the report: the
            // wake promise ("an input sent by the agent owes it an output")
            // parks the delivery for the requester's drain / the App's
            // idle-main-agent wake, exactly like background mode.
            match state.waits.remove(&request.id) {
                Some(pending) => {
                    // `send` hands the value BACK when the receiver is gone
                    // — a dropped wait handle means the requester's turn
                    // ended before the report; the promise parks it.
                    if let Err(undelivered) = pending.tx.send(output) {
                        state.deliveries.push(PaneDelivery {
                            pane: pane.to_string(),
                            request_id: request.id,
                            requester,
                            output: undelivered,
                            kind: DeliveryKind::CallReport,
                        });
                    }
                }
                None => state.deliveries.push(PaneDelivery {
                    pane: pane.to_string(),
                    request_id: request.id,
                    requester,
                    output,
                    kind: DeliveryKind::CallReport,
                }),
            }
        }
        ReplyMode::Background => state.deliveries.push(PaneDelivery {
            pane: pane.to_string(),
            request_id: request.id,
            requester,
            output,
            kind: DeliveryKind::CallReport,
        }),
    }
    Some(requester)
}

/// Fail a pane's IN-FLIGHT request, if any: the pane's turn was force-
/// closed (the Ctrl+X interrupt) or reset without completing — a leftover
/// serving entry would keep [`has_outstanding_requests`] true forever and
/// hang the requester's busy-guard at its natural-completion point.
/// Queued (not yet served) requests are kept: the freed pane can still
/// serve them.
pub fn fail_serving(pane: &str, error: String) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(request) = state.serving.remove(pane) {
        fail_wait(&mut state, &request.id, error);
    }
}

/// Fail ALL of a pane's outstanding requests — in-flight AND queued — and
/// drop the pane from the registry entirely. For a pane set that dies as a
/// WHOLE (a session switch, the app quitting): the pane will never serve
/// anything again in this process, so a leftover queued request would hang
/// its requester's busy-guard forever (`has_outstanding_requests` stuck
/// true with nobody left to dispatch it) — the same terminal state a
/// leftover serving entry causes, one step earlier in the pipeline. Unlike
/// [`fail_serving`] (single in-flight turn) and [`set_panes`] (set
/// replacement, where surviving panes keep serving), this is the
/// point-of-no-return failure.
pub fn fail_pane(pane: &str, error: String) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(request) = state.serving.remove(pane) {
        fail_wait(&mut state, &request.id, error.clone());
    }
    if let Some(queue) = state.queues.remove(pane) {
        for request in queue {
            fail_wait(&mut state, &request.id, error.clone());
        }
    }
    state.panes.retain(|p| p.id != pane);
    state.deliveries.retain(|d| d.pane != pane);
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
/// for a pane to free) or in-flight (a pane is working on it) — or any
/// PARKED worktree op of its own. The agent loop's busy-guard reads this at
/// its natural-completion point: a turn that asked a pane for something
/// does NOT end while the answer is still outstanding — ending it would
/// orphan the wait and lose the report.
///
/// A parked `agents_worktree` op counts for the same reason — but only on
/// the EXCEPTION path. Normally the asking tool call pauses inside
/// `dispatch_next` until the App resolves the ask, so by the time the turn
/// can end the outcome is already in the model's hands. What still parks
/// here is a stop-interrupted ask: the OUTCOME (executed, refused by the
/// user, failed in git) is then still owed to this loop as a Worktree
/// delivery, and ending the turn would leave it to a later, unrelated turn
/// — or never, if the session closes. Ops are cheap to wait for (the App
/// drains them every frame) and a user permission box answers promptly, so
/// this cannot hang the loop.
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
        || state.ops.iter().any(|op| op.requester == requester)
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
                kind: delivery.kind,
            });
            false
        } else {
            true
        }
    });
    ready
}

/// Take the ONE Worktree-kind delivery addressed to `requester` with the
/// given op id, without touching any other parked delivery. The paused
/// `agents_worktree` dispatch polls this — `take_ready_deliveries` would
/// also sweep up unrelated `agents_call` reports sitting in the same
/// queue and drop them (the loop would never see those reports).
/// `None` while the ask is still pending.
#[must_use]
pub fn take_ready_worktree_delivery(requester: &str, request_id: &str) -> Option<PaneDelivery> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let index = state.deliveries.iter().position(|delivery| {
        delivery.requester == requester
            && delivery.request_id == request_id
            && delivery.kind == DeliveryKind::Worktree
    })?;
    Some(state.deliveries.remove(index))
}

/// Render one finished delivery as the `[automated notification]` user
/// turn both loop drain sites inject (the per-turn drain and the pre-Done
/// busy-guard): same contract, same shape — the body differs per kind (a
/// pane report vs a worktree binding update).
pub fn delivery_note(delivery: &PaneDelivery) -> String {
    let report = match &delivery.output {
        Ok(report) => super::background::sanitize(report),
        Err(error) => error.clone(),
    };
    match delivery.kind {
        DeliveryKind::Worktree => format!(
            "[automated notification] Worktree binding update for pane '{}' (agents_worktree {}):\n{report}",
            delivery.pane, delivery.request_id
        ),
        DeliveryKind::CallReport => format!(
            "[automated notification] Pane '{}' (agents_call {}) finished.\nReport:\n{}",
            delivery.pane, delivery.request_id, report
        ),
    }
}

/// Park one `agents_worktree` ask — the mirror of [`enqueue`]'s contract,
/// minus the wait-mode half. The op is FIFO-queued in the registry until
/// the App's frame drain pops it; the asking tool call then pauses inside
/// `dispatch_next` and consumes the outcome itself
/// ([`take_ready_worktree_delivery`]), so the tool result IS the truth.
/// Only a stop-interrupted ask falls back to a Worktree-kind delivery in
/// the requester's loop drain on a later turn.
///
/// The TARGET rule is uniform: every pane in the set is addressable, the
/// cosh (`is_manager`) ones included — a cosh pane runs a real agent session
/// with its own cwd, so isolating one is a legitimate ask exactly like
/// isolating a `claude` pane, and it is the supremacy check (a
/// User-assigned binding always opens the user's permission box) plus the
/// pane-stripping (`with_own_pane_id` removes this tool from every pane
/// loop) that keep the user in charge. The old blanket refusal of cosh panes
/// made `/worktree` the ONLY way to isolate them, which is exactly the
/// asymmetry that read as "the command is missing for cosh panes".
///
/// # Errors
///
/// An unknown pane id (the model guessed a name that is not bound — the
/// error names the known ids so the model can correct itself), an `Assign`
/// without a usable `name`, or a `name` the worktree engine would refuse at
/// its git argument (leading dash, whitespace, control characters) —
/// refused HERE so nothing parks.
pub fn enqueue_worktree_op(
    pane: &str,
    action: WorktreeAction,
    name: Option<&str>,
    requester: &'static str,
) -> Result<String, String> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    let known: Vec<String> = state.panes.iter().map(|p| p.id.clone()).collect();
    if !state.panes.iter().any(|p| p.id == pane) {
        return Err(format!(
            "no pane '{pane}' in this session's /agents split; known panes: {known:?}"
        ));
    }
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if action == WorktreeAction::Assign {
        let Some(name) = name else {
            return Err(
                "`agents_worktree` assign requires `name` — the branch the new worktree checks \
                 out (its slug also names the worktree directory)."
                    .to_string(),
            );
        };
        // The ENGINE's rule, called rather than restated: same acceptance
        // set, same sentence, one place to change it. Refused BEFORE
        // parking so the tool result is the immediate refusal and the
        // model corrects itself in place.
        if let Err(error) = crate::git::worktree::validate_name(name) {
            return Err(error.to_string());
        }
    }
    state.next_op += 1;
    let id = format!("wt-op-{}", state.next_op);
    let op = WorktreeOp {
        id: id.clone(),
        pane: pane.to_string(),
        action,
        branch: name.map(str::to_string),
        requester,
    };
    state.ops.push_back(op);
    Ok(id)
}

/// Look at the OLDEST parked worktree op without consuming it — the App's
/// drain needs the ask to DECIDE (supremacy check, modal availability)
/// before popping. `None` when nothing is parked.
#[must_use]
pub fn peek_worktree_op() -> Option<WorktreeOp> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ops
        .front()
        .cloned()
}

/// Consume the OLDEST parked worktree op — after the App's decision is
/// final (execute now, or the user's confirmation answered). The popped
/// op pairs with [`complete_worktree_op`]'s outcome by its id. The App is
/// the ONLY consumer; `None` when the registry head moved (nothing or
/// someone else's drain).
#[must_use]
pub fn pop_worktree_op() -> Option<WorktreeOp> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    state.ops.pop_front()
}

/// Deliver a worktree op's outcome to its requester's loop drain: the
/// tool parked the ask and is PAUSED on it (its dispatch polls for this
/// delivery as its own tool result), so this is the outcome's FIRST
/// consumer path. If the requester's wait was interrupted by a stop, the
/// delivery stays parked instead and reaches the loop through its drain
/// (same path as a background `agents_call` report — [`delivery_note`]
/// renders the automated notification).
pub fn complete_worktree_op(op: &WorktreeOp, outcome: Result<String, String>) {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    state.deliveries.push(PaneDelivery {
        pane: op.pane.clone(),
        request_id: op.id.clone(),
        requester: op.requester,
        output: outcome,
        kind: DeliveryKind::Worktree,
    });
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
         by its pane id (`agents_call` to message one, `agents_worktree` to \
         move one into or out of a git worktree):\n",
    );
    for pane in &panes {
        let session = if pane.bound {
            "agent session bound"
        } else {
            "no session yet (the first agents_call wakes it)"
        };
        let worktree = pane
            .worktree
            .as_deref()
            .map(|name| format!(", worktree {name}"))
            .unwrap_or_default();
        let _ = writeln!(
            block,
            "- {} — {} ({session}{worktree})",
            pane.id, pane.agent
        );
    }
    Some(block)
}

/// The awareness block a PANE sees: the pane set WITHOUT the coordination
/// vocabulary. A pane must never learn the `agents_call` ids or its own
/// addressability (the replication bug `with_own_pane_id` documents) — but
/// it still benefits from knowing WHICH siblings share its workspace, so
/// it can mention them ("claude is on the same branch") instead of assuming
/// it is alone. Rendered for the manager pane's context tail (Phase 1) and
/// as the preamble text for the ACP panes (Phase 3's prompt prefix).
#[must_use]
pub fn peer_block(own_id: &str) -> Option<String> {
    use std::fmt::Write as _;
    let panes = list_panes();
    // Siblings only: the pane itself is not a peer.
    let siblings: Vec<&PaneDescriptor> = panes.iter().filter(|p| p.id != own_id).collect();
    if siblings.is_empty() {
        return None;
    }
    let mut block = String::from(
        "## Agents Panes\n\
         Other /agents panes share this session's workspace alongside you:\n",
    );
    for pane in &siblings {
        let worktree = pane
            .worktree
            .as_deref()
            .map(|name| format!(", worktree {name}"))
            .unwrap_or_default();
        let _ = writeln!(block, "- {} — {}{}", pane.agent, pane.id, worktree);
    }
    block.push_str(
        "You do NOT coordinate them (no `agents_call` for you): do your own \
         delegated work and report back — the main agent owns the coordination.",
    );
    Some(block)
}

/// Take the awareness preamble for an ACP pane, when its announcement went
/// stale. Returns the text to PREPEND to the pane's next prompt, and marks
/// the pane as announced at the current generation; `None` when the pane is
/// already up to date (nothing rides the prompt). ACP has no system prompt,
/// so the pane's awareness can only arrive as prompt text — this keeps it
/// once-per-change instead of once-per-turn.
pub fn take_preamble(pane_id: &str) -> Option<String> {
    let mut state = registry().lock().unwrap_or_else(|p| p.into_inner());
    if !state.panes.iter().any(|p| p.id == pane_id) {
        return None;
    }
    if state.announced.get(pane_id) == Some(&state.generation) {
        return None;
    }
    // Announce exactly the peer view (same text the manager pane sees).
    let block = {
        let siblings: Vec<String> = state
            .panes
            .iter()
            .filter(|p| p.id != pane_id)
            .map(|p| {
                let worktree = p
                    .worktree
                    .as_deref()
                    .map(|name| format!(", worktree {name}"))
                    .unwrap_or_default();
                format!("- {} — {}{}", p.agent, p.id, worktree)
            })
            .collect();
        if siblings.is_empty() {
            None
        } else {
            Some(format!(
                "## Agents Panes\n\
                 Other /agents panes share this session's workspace alongside you:\n{}\n\
                 You do NOT coordinate them: do your own delegated work and report back — \
                 the main agent owns the coordination.",
                siblings.join("\n")
            ))
        }
    };
    let generation = state.generation;
    state.announced.insert(pane_id.to_string(), generation);
    block
}

/// The two shapes both pane tools need from the CURRENT pane set: the ids
/// joined for the description ("claude, kilo"), and the same ids as the
/// JSON-schema `enum` for the `pane` property. Built once here because both
/// tools address the SAME ids — a second copy is how the description and
/// the enum drift apart.
#[must_use]
fn pane_schema_parts() -> (String, Vec<serde_json::Value>) {
    let ids: Vec<String> = list_panes().iter().map(|p| p.id.clone()).collect();
    let pane_list = ids.join(", ");
    let pane_enum = ids
        .into_iter()
        .map(serde_json::Value::String)
        .collect::<Vec<_>>();
    (pane_list, pane_enum)
}

/// The `agents_call` harness tool, built against the CURRENT pane set:
/// the description lists the addressable ids (the model never guesses)
/// and the schema's `pane` enum carries the same ids. Called by the
/// harness builder only when panes exist — with none, the tool is never
/// registered and no context carries it.
#[must_use]
pub fn agents_call_tool() -> super::core::HarnessTool {
    let (pane_list, pane_enum) = pane_schema_parts();
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
                    "enum": pane_enum,
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

/// The `agents_worktree` harness tool, built against the CURRENT pane set:
/// the same addressable ids `agents_call` speaks, the cosh (`is_manager`)
/// panes INCLUDED — a cosh pane runs a real agent session with its own cwd,
/// so the agent may ask to isolate it exactly like any other pane, and the
/// user keeps the final word through the session permission box (Allow /
/// Allow Once / Deny) a User-assigned binding always opens. Registered ONLY
/// when panes exist and
/// stripped from every pane loop by `with_own_pane_id` — so a pane can
/// never rebind itself or a peer. Worktrees stay opt-in (the agent asks, the
/// user can always veto a User-assigned pane's rebinding).
#[must_use]
pub fn agents_worktree_tool() -> super::core::HarnessTool {
    let (pane_list, pane_enum) = pane_schema_parts();
    super::core::HarnessTool {
        name: "agents_worktree".into(),
        description: format!(
            "Move one of this session's /agents panes into (or out of) a git worktree, so it \
             works on its own branch without touching the session's working tree.\n\
             `pane` is the addressable pane id ({pane_list}); `action` is `assign` (isolate the \
             pane in a NEW temp worktree under the system temp dir, checking out branch \
             `name` from the session repo's current HEAD) or `clear` (drop the binding — the \
             pane returns to the session's working directory; the worktree and its branch are \
             KEPT on disk, never removed here).\n\
             ## Rules\n\
             - `name` (required on `assign`): the branch to check out. Its slug also names the \
             worktree directory. Collisions auto-suffix (`-2`, `-3`, …).\n\
             - NO path parameter: the worktree location is cosh's temp root — the user picks \
             explicit locations via their own `/worktree` command, never you.\n\
             - The pane (and you) learn the new cwd automatically before the pane's next task: \
             include this in your delegation message when you assign one.\n\
             - The pane's UNCOMMITTED work is not carried along — assign a pane BEFORE it \
             starts editing, or after it committed.\n\
             - Rebinding a pane the USER assigned to a worktree opens the session permission \
             box (Allow / Allow Once / Deny) the user must answer — their binding, their call \
             (even in autonomous runs) — and THIS tool call pauses until they do. The call's \
             result IS the outcome: success text on Allow, a refusal explanation on Deny. If \
             the turn is stopped while the box is open, the ask stays parked and its outcome \
             arrives later as an [automated notification]."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "pane": {
                    "type": "string",
                    "description": "The pane to (re)bind. Must be one of the ids listed in the description.",
                    "enum": pane_enum,
                },
                "action": {
                    "type": "string",
                    "enum": ["assign", "clear"],
                    "description": "assign = isolate the pane in a new temp worktree on branch `name`; clear = drop the binding (nothing is deleted on disk).",
                },
                "name": {
                    "type": "string",
                    "description": "Required on `assign`: the branch name the new worktree checks out (also names the worktree directory). Ignored on clear.",
                },
            },
            "required": ["pane", "action"],
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
            // flag from the manager-pane label, mirrored here for the tests.
            is_manager: agent == "cosh",
            bound,
            worktree: None,
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
        assert_eq!(
            complete_request("claude", Ok("done".to_string())),
            Some(MAIN),
            "the completion reports its requester — the App's wake promise reads it"
        );
        let answer = block_on(async move { rx.await.expect("sender alive") });
        assert_eq!(answer, Ok("done".to_string()));
    }

    /// The point-of-no-return failure (a session switch / going Home): a
    /// pane that will never serve again must lose its in-flight AND queued
    /// requests — every waiter unblocks with the failure, the pane leaves
    /// the registry (`has_panes` drops), and no queued entry survives to
    /// hang a busy-guard (`has_outstanding_requests` false) or be served
    /// cross-session by a later pane reusing the same id.
    #[test]
    fn fail_pane_clears_every_outstanding_request_and_the_descriptor() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        // One in-flight (the App dispatched it) + one still queued wait,
        // and one background request parked for its drain.
        let inflight = enqueue("claude", "working on it", MAIN, ReplyMode::Wait).unwrap();
        let queued = enqueue("claude", "waiting its turn", MAIN, ReplyMode::Wait).unwrap();
        let queued_rx = queued.receiver.expect("wait mode carries a receiver");
        assert!(pop_next_request("claude").is_some(), "dispatched");
        let _background = enqueue("claude", "fire and forget", MAIN, ReplyMode::Background).unwrap();

        fail_pane("claude", "the session switched away".to_string());

        // Every waiter resolved with the failure — the tool's `select!`
        // unblocks instead of hanging.
        assert_eq!(
            block_on(async move {
                inflight
                    .receiver
                    .expect("wait")
                    .await
                    .expect("sender alive")
            }),
            Err("the session switched away".to_string())
        );
        assert_eq!(
            block_on(async move { queued_rx.await.expect("sender alive") }),
            Err("the session switched away".to_string())
        );
        // Nothing outstanding on either side, nothing drainable, no pane.
        assert!(!has_outstanding_requests(MAIN));
        assert!(take_ready_deliveries(MAIN).is_empty());
        assert!(!has_panes());
        assert!(
            enqueue("claude", "again", MAIN, ReplyMode::Wait).is_err(),
            "the dead pane id no longer resolves"
        );
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
        assert_eq!(
            complete_request("claude", Ok("report".to_string())),
            Some(MANAGER)
        );
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
    fn worktree_op_parks_fifo_and_delivers_to_its_requester() {
        let _g = lock();
        set_panes(vec![
            pane("claude", "claude", false),
            pane("kilo", "kilo", false),
        ]);
        let op1 = enqueue_worktree_op("claude", WorktreeAction::Assign, Some("fix-1"), MAIN)
            .expect("parks");
        let op2 = enqueue_worktree_op("kilo", WorktreeAction::Clear, None, MANAGER).expect("parks");
        assert_ne!(op1, op2, "ids are unique");
        // FIFO across panes: the FIRST parked ask pops first, whichever
        // pane it targets.
        assert_eq!(peek_worktree_op().unwrap().id, op1);
        let popped = pop_worktree_op().expect("the head");
        assert_eq!(popped.id, op1);
        assert_eq!(popped.pane, "claude");
        assert_eq!(popped.action, WorktreeAction::Assign);
        assert_eq!(popped.branch.as_deref(), Some("fix-1"));
        assert_eq!(popped.requester, MAIN);
        // The outcome delivery routes to the op's OWN requester only.
        complete_worktree_op(&popped, Ok("isolated".to_string()));
        let drained = take_ready_deliveries(MAIN);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].request_id, op1);
        assert_eq!(drained[0].kind, DeliveryKind::Worktree);
        assert_eq!(drained[0].output.clone().unwrap(), "isolated");
        assert!(
            take_ready_deliveries(MANAGER).is_empty(),
            "wrong loop never drains"
        );
        // The second op is now the head.
        assert_eq!(peek_worktree_op().unwrap().id, op2);
    }

    #[test]
    fn worktree_op_validation_refuses_unknown_and_bad_names() {
        let _g = lock();
        set_panes(vec![
            pane("cosh", "cosh", true),
            pane("claude", "claude", false),
        ]);
        // Unknown pane: the error names the known ids.
        let err = enqueue_worktree_op("ghost", WorktreeAction::Clear, None, MAIN).unwrap_err();
        assert!(err.contains("ghost") && err.contains("claude"), "{err}");
        // A cosh (manager) pane IS an address: the tool covers every pane kind
        // now — isolating one is the same ask as isolating a `claude` pane,
        // and the User-assigned confirmation box is what protects the user's
        // own binding. The ask parks.
        let id = enqueue_worktree_op("cosh", WorktreeAction::Assign, Some("cosh-iso"), MAIN)
            .expect("a cosh pane is addressable");
        assert_eq!(peek_worktree_op().expect("parked").id, id);
        // Drain it so the later "refusals park nothing" assertion is about
        // refusals only.
        let _ = pop_worktree_op();
        // Assign without a name: refused, nothing parked.
        assert!(enqueue_worktree_op("claude", WorktreeAction::Assign, None, MAIN).is_err());
        assert!(peek_worktree_op().is_none());
        // A name that would never survive a git argument: refused here.
        assert!(
            enqueue_worktree_op("claude", WorktreeAction::Assign, Some("-evil"), MAIN).is_err()
        );
        let spaced =
            enqueue_worktree_op("claude", WorktreeAction::Assign, Some("a b"), MAIN).unwrap_err();
        // The engine's own sentence, not a second wording of the same rule.
        assert_eq!(
            spaced,
            crate::git::worktree::validate_name("a b")
                .unwrap_err()
                .to_string(),
            "the registry defers to the engine's validation text"
        );
        assert!(peek_worktree_op().is_none(), "refusals park nothing");
    }

    /// A PARKED worktree op is outstanding work for its requester — the
    /// busy-guard must hold the turn open until the outcome delivery lands,
    /// exactly as it does for a queued/in-flight `agents_call`.
    #[test]
    fn a_parked_worktree_op_is_outstanding_until_its_outcome_is_delivered() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        assert!(!has_outstanding_requests(MAIN), "nothing to wait for yet");
        enqueue_worktree_op("claude", WorktreeAction::Assign, Some("wt-f7"), MAIN).expect("parks");
        assert!(
            has_outstanding_requests(MAIN),
            "the parked op's outcome is still owed to this loop"
        );
        assert!(
            !has_outstanding_requests(MANAGER),
            "another loop is not held by this op"
        );
        // The App executes it and delivers the outcome: no longer outstanding.
        let op = pop_worktree_op().expect("the parked op");
        complete_worktree_op(&op, Ok("isolated in /tmp/wt-f7".to_string()));
        assert!(
            !has_outstanding_requests(MAIN),
            "a delivered outcome is finished work"
        );
        assert_eq!(take_ready_deliveries(MAIN).len(), 1, "the outcome drains");
    }

    #[test]
    fn worktree_ops_of_removed_panes_fail_without_losing_the_outcome() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        let op = enqueue_worktree_op("claude", WorktreeAction::Assign, Some("fix"), MAIN)
            .expect("parks");
        // The pane dies with a parked ask: the ask is answered (a failure)
        // so the asking loop's drain still completes its pairing.
        set_panes(vec![pane("kilo", "kilo", false)]);
        assert!(peek_worktree_op().is_none(), "the dead pane's op is gone");
        let deliveries = take_ready_deliveries(MAIN);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].request_id, op);
        assert_eq!(deliveries[0].kind, DeliveryKind::Worktree);
        assert!(deliveries[0].output.is_err());
        assert!(deliveries[0].output.clone().unwrap_err().contains("claude"));
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
        assert_eq!(
            complete_request("claude", Ok("report".to_string())),
            Some(MANAGER)
        );
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

    /// Issue 5 regression: a leftover `serving` entry on a pane the App is
    /// about to dispatch to is ORPHANED — its turn never completed (the
    /// dispatch was refused after the pop, or the terminal Done was
    /// stale-dropped). `pop_next_request` must fail that stale wait so the
    /// requester's busy-guard unblocks instead of hanging forever, while
    /// the freshly popped request serves normally.
    #[test]
    fn pop_next_request_fails_an_orphaned_serving_entry() {
        let _g = lock();
        set_panes(vec![pane("cosh#1", "cosh", false)]);
        let lost = enqueue("cosh#1", "lost turn", MAIN, ReplyMode::Wait).unwrap();
        let lost_rx = lost.receiver.expect("receiver");
        assert!(pop_next_request("cosh#1").is_some(), "dispatched");
        // The turn died BEFORE its completion: the serving entry survives.
        // The App pops again for the pane's NEXT request — the orphaned
        // wait fails, and the new request takes the serving slot.
        let next = enqueue("cosh#1", "next turn", MAIN, ReplyMode::Wait).unwrap();
        let next_rx = next.receiver.expect("receiver");
        let popped = pop_next_request("cosh#1").expect("next queued request");
        assert_eq!(popped.text, "next turn");
        let answer = block_on(async move { lost_rx.await.expect("sender alive") });
        assert_eq!(
            answer,
            Err("pane 'cosh#1' orphaned the request: its turn never completed".to_string()),
            "the orphaned turn's wait is failed by the next pop"
        );
        assert!(
            has_outstanding_requests(MAIN),
            "the NEW request is outstanding (serving) until it completes"
        );
        assert_eq!(
            complete_request("cosh#1", Ok("done".to_string())),
            Some(MAIN)
        );
        let ok = block_on(async move { next_rx.await.expect("sender alive") });
        assert_eq!(ok, Ok("done".to_string()));
        assert!(!has_outstanding_requests(MAIN));
    }

    /// Issue 5 regression: `fail_serving` is the teardown-side release —
    /// the pane's in-flight wait resolves with the given reason and the
    /// registry shows no outstanding work (the manager loop's busy-guard
    /// can reach its Done instead of waiting forever).
    #[test]
    fn fail_serving_releases_the_in_flight_wait() {
        let _g = lock();
        set_panes(vec![pane("cosh#1", "cosh", false)]);
        let handle = enqueue("cosh#1", "in flight", MAIN, ReplyMode::Wait).unwrap();
        let rx = handle.receiver.expect("receiver");
        assert!(pop_next_request("cosh#1").is_some(), "dispatched");
        assert!(
            has_outstanding_requests(MAIN),
            "the wait is outstanding while served"
        );
        fail_serving("cosh#1", "the pane's set was replaced or torn down".into());
        let answer = block_on(async move { rx.await.expect("sender alive") });
        assert_eq!(
            answer,
            Err("the pane's set was replaced or torn down".to_string())
        );
        assert!(
            !has_outstanding_requests(MAIN),
            "no outstanding requests survive the failure"
        );
        // A second failure is a no-op: the entry is gone.
        fail_serving("cosh#1", "again".into());
        assert!(!has_outstanding_requests(MAIN));
    }

    /// Phase 1: the worktree name rides the main agent's pane listing next
    /// to the session state, so the agent always knows where each pane
    /// works; a pane without one renders exactly as before (no suffix).
    #[test]
    fn context_block_renders_the_worktree_next_to_the_session() {
        let _g = lock();
        let mut pinned = pane("claude", "claude", true);
        pinned.worktree = Some("wt-feat".to_string());
        set_panes(vec![pinned, pane("kilo", "kilo", false)]);
        let block = context_block().expect("block");
        assert!(
            block.contains("- claude — claude (agent session bound, worktree wt-feat)"),
            "{block}"
        );
        assert!(
            block.contains("- kilo — kilo (no session yet (the first agents_call wakes it))"),
            "no-worktree pane renders unchanged: {block}"
        );
    }

    /// Phase 1: the pane's awareness is the SIBLING view — it never sees
    /// itself and never sees coordination vocabulary; a pane with no
    /// siblings gets no block at all.
    #[test]
    fn peer_block_shows_only_siblings_without_coordination() {
        let _g = lock();
        set_panes(vec![pane("claude", "claude", false)]);
        assert!(
            peer_block("claude").is_none(),
            "a lone pane has no peers to announce"
        );
        let mut pinned = pane("kilo", "kilo", true);
        pinned.worktree = Some("wt-fix".to_string());
        set_panes(vec![pane("claude", "claude", false), pinned]);
        let block = peer_block("claude").expect("siblings exist");
        assert!(block.starts_with("## Agents Panes"), "{block}");
        assert!(block.contains("- kilo — kilo, worktree wt-fix"), "{block}");
        assert!(
            !block.contains("claude"),
            "the pane never sees itself: {block}"
        );
        assert!(
            block.contains("You do NOT coordinate them"),
            "the no-coordination contract rides the block: {block}"
        );
        assert!(
            !block.contains("address each"),
            "no coordination vocabulary leaks into a pane's awareness: {block}"
        );
    }

    /// Phase 1: the ACP preamble rides the prompt once per pane-set change,
    /// not once per turn — a second take at the same generation is empty,
    /// a `set_panes` (any change: binding, worktree, membership) re-arms
    /// it, and a removed pane stops being announceable.
    #[test]
    fn take_preamble_announces_once_per_generation() {
        let _g = lock();
        set_panes(vec![
            pane("claude", "claude", false),
            pane("kilo", "kilo", false),
        ]);
        let first = take_preamble("claude").expect("first dispatch announces");
        assert!(first.contains("- kilo — kilo"), "{first}");
        assert!(
            !first.contains("claude"),
            "no self in the preamble: {first}"
        );
        assert!(
            take_preamble("claude").is_none(),
            "same generation → nothing rides the prompt again"
        );
        // Any set_panes bumps the generation — the next dispatch re-announces.
        set_panes(vec![
            pane("claude", "claude", true),
            pane("kilo", "kilo", false),
        ]);
        let second = take_preamble("claude").expect("a changed pane set re-arms the preamble");
        assert!(second.contains("- kilo — kilo"), "{second}");
        // A pane gone from the split cannot be announced; re-adding it
        // (generation bump, announce entry pruned) re-arms it from scratch.
        set_panes(vec![pane("kilo", "kilo", false)]);
        assert!(
            take_preamble("claude").is_none(),
            "a removed pane is not announceable"
        );
        set_panes(vec![
            pane("claude", "claude", false),
            pane("kilo", "kilo", false),
        ]);
        assert!(
            take_preamble("claude").is_some(),
            "a re-added pane announces from scratch"
        );
        // A lone pane still consumes its announcement (nothing to say, but
        // the bookkeeping keeps it up to date for when a sibling appears).
        set_panes(vec![pane("claude", "claude", false)]);
        assert!(
            take_preamble("claude").is_none(),
            "no siblings → no preamble text"
        );
    }
}
