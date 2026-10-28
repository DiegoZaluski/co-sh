//! Tuning knobs, loop-detection bounds and internal marker constants.

/// Maximum consecutive tool-call failures before aborting the agent loop.
pub(super) const MAX_TOOL_RETRIES: usize = 10;

/// Maximum total agent-loop iterations (tool calls + responses) before
/// the harness stops the loop as a safety net against runaway tool-calling.
///
/// 20 was too low for real multi-step work (editing files, running tests,
/// searching) — the loop force-stopped mid-task with a misleading `Done`.
pub(crate) const MAX_ITERATIONS: u64 = 100;

/// How many times the MAX_ITERATIONS guard may RENEW the iteration budget.
/// The hard limit exists to kill a RUNAWAY loop — and the loop detectors
/// (tool-signature window, textual repetition) already cover runaway: a loop
/// whose recent window shows no stuck repetition is making real progress, and
/// long multi-step work is legitimate. When the guard fires while the window
/// is clean, the budget renews (bounded) instead of cutting a healthy turn
/// with tool calls still queued; a stuck or renewal-exhausted loop stops
/// visibly (warn + toast) as before.
pub(crate) const ITERATION_RENEWALS: u32 = 2;

/// Natural-stop gate budget: how many continuations the deterministic veto
/// layer (reasoning-only retry, empty-response retry) may grant per agent
/// loop. An agent loop runs for hours and each veto is a REAL provider cut
/// the model must recover from — 3 proved too tight: a model prone to
/// reasoning-only `stop` streams (e.g. glm-5.3-flash) exhausts the budget
/// mid-task and the loop then accepts the bare `stop` silently (Done with
/// `checkup_verdict: None`, no toast, no trace). 10 is the measured starting
/// point: generous enough to absorb a streak of degenerate stops, still
/// scarce enough that a model insisting on stopping terminates visibly
/// instead of looping to `MAX_ITERATIONS`. If the silent-exhaustion pattern
/// resurfaces, raise this — never remove the bound.
pub(crate) const STOP_NUDGE_BUDGET: u32 = 10;

/// Resume budget for a `/agents` PANE turn — how many times the harness
/// re-sends the context after a stop NOBODY asked for (the natural-stop
/// gate's [`STOP_NUDGE_BUDGET`] exhausted on an empty completion, or the
/// textual repetition detector firing).
///
/// Why a pane needs this and a chat turn does not: a chat turn's stop is
/// final (the user reads the answer, or asks again), while a pane turn owes a
/// DELIVERABLE — the report its `agents_call` requester is blocked on — and
/// `stop_agent_loop` is blocked for panes ([`SUBAGENT_BLOCKED_TOOLS`] here,
/// the TUI's `MANAGER_BLOCKED_TOOLS` for the cosh pane), so the loop cannot
/// end early on purpose. The only legitimate early end is the USER's stop,
/// which is exactly observable (`self.stop` / the shared `stop_signal` — the
/// same flags `check_stop!` reads). Any other stop is an IMPROPER stop: the
/// pane hands its requester `incomplete` while the model may have been cut one
/// turn before writing the report.
///
/// SCOPE — this budget covers the two shapes that produced the field reports
/// (an exhausted empty stop, and the repetition detector). It deliberately
/// does NOT cover every terminal shape: `MAX_ITERATIONS`, `MAX_TOOL_RETRIES`
/// and the tool-interaction loop detector still stop a pane (and report
/// `incomplete` with their guard verdict). Those are cost/safety nets whose
/// stop is a bound on RESOURCE EXHAUSTION, not a verdict on the work — the
/// model has provably failed the task those bounds exist to catch, so
/// granting more context re-sends would buy nothing. Widening this to them
/// would turn a runaway model into a longer runaway.
///
/// Continuing is safe because it is the SAME mechanism every other
/// continuation in the loop uses — full history plus one steering turn, the
/// provider-standard re-send — not a new wire shape and not a fabricated user
/// turn. What was missing was only the BOUND: the loop's nudges renew per
/// ENTRY (`run_agent_loop_inner` resets `stop_nudges_left`), so without a
/// per-loop budget a resuming pane could renew forever. Two is enough to
/// recover a cut turn (the observed defect stops after ONE) and short enough
/// that a chronically empty model still terminates visibly, reported as
/// `incomplete` with its guard verdict. Never remove this bound; raise it
/// only against a measured streak.
pub(crate) const COWORKER_RESUME_BUDGET: u32 = 2;

/// Loop-detection window: the number of recent tool-call iterations watched
/// for a repeated identical interaction, mirroring crush's
/// `loopDetectionWindowSize`. Detection only fires once the window is full
/// (at least this many tool-calling iterations have happened).
pub(super) const LOOP_DETECTION_WINDOW_SIZE: usize = 10;

/// Loop-detection threshold: an identical tool+input+result signature seen
/// more than this many times within the window stops the loop, mirroring
/// crush's `loopDetectionMaxRepeats`. Catches a model hammering the exact
/// same failing call long before it burns tokens up to `MAX_ITERATIONS`.
pub(super) const LOOP_DETECTION_MAX_REPEATS: usize = 5;

/// Repetition-detection threshold: consecutive iterations whose TEXTUAL
/// output (visible answer or reasoning-only thinking) is identical stop the
/// loop. The crush-style window above only sees TOOL interactions — it is
/// blind to a model stuck repeating the same thought with no tool call at
/// all (observed in the field: a reasoning-only loop repeated the same
/// paragraph for dozens of iterations while the natural-stop gate kept
/// continuing it, every continuation re-sending the ever-growing context).
/// Reasoning text is usually byte-stable across repeats (the provider
/// re-derives the same thought), so a tight bound of 2 identical turns is
/// safe: legitimate work almost never repeats a full turn verbatim twice,
/// and a model that DOES vary its wording still hits the tool-signature
/// detector or `MAX_ITERATIONS`.
pub(super) const REPETITION_MAX_IDENTICAL: usize = 2;

/// Minimum length of normalized text eligible for repetition detection.
/// Short texts are skipped: a tiny generic reply ("ok", "done.") carries too
/// little signal to prove the model is stuck and is cheap to re-send anyway.
pub(super) const REPETITION_MIN_TEXT_LEN: usize = 64;

/// Tools disabled inside the internal sub-agent harness (the merged
/// `subagent_call` internal path). The tools are removed from the
/// sub-agent's schema AND extractor, so the model never even sees them:
/// - `ask_questions` — needs a human in the loop; the sub-agent is headless
///   and would hang waiting for an answer;
/// - `stop_agent_loop` — the sub-agent's ONLY deliverable is its final text
///   report. If it could stop the loop directly, a model might call it as
///   its last action without writing the report (the caller would receive
///   nothing). Blocked so the loop can only end with a written answer.
///
/// Add future blocklist entries here.
pub(crate) const SUBAGENT_BLOCKED_TOOLS: &[&str] = &["ask_questions", "stop_agent_loop"];

/// Chunk interpolated into the `subagent_call` tool description (via
/// [`SubAgent::set_note`]) so the model learns — naturally, inside the
/// description prose — that omitting `agent` routes the call to an
/// internal agent instead of an external CLI.
pub(super) const SUBAGENT_INTERNAL_NOTE: &str = "When the `agent` argument is omitted or empty, an internal agent runs \
     the task instead — it starts with an empty context, runs in auto-approve \
     mode, persists nothing, and returns only its final report.";

/// Internal marker returned by stream functions when the user presses Esc
/// to stop the current generation. Compared by identity (constant), not by
/// string value — prevents the fallback retry from misinterpreting a user
/// cancellation as a connector error.
pub(crate) const INTERRUPTED_MARKER: &str = "__cosh_interrupted__";

/// Internal marker returned by a stream function when the provider rejected
/// the request because the prompt exceeds its context window. Compared by
/// identity (constant), like [`INTERRUPTED_MARKER`] — the harness drains one
/// tool chain and retries when it sees this. `pub(crate)` so tests can
/// simulate the overflow through the mock paths.
pub(crate) const CONTEXT_WINDOW_MARKER: &str = "__cosh_context_window_exceeded__";
