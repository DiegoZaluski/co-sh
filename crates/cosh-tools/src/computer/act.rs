//! `computer_act` — semantic actions AND keyboard as one pipeline on the
//! accessibility tree (no screen coordinates involved).
//!
//! One step is EITHER a semantic action on the element matched by
//! `selector` (auto-waiting for visible + enabled), OR a keyboard action
//! (`key`/`text` via the SHARED keyboard engine), OR a `wait` pause — and
//! steps chain via `then` with shell `&&` semantics, each only if the
//! previous succeeded, the first failure aborting the chain with the exact
//! failing step and everything that completed.
//!
//! This file is ONLY the pipeline: chain walking, wait steps and step
//! classification. The step KINDS live in their components — semantic
//! dispatch in [`super::touch`], keyboard dispatch in [`super::keyboard`]
//! — so each can be reused by other tools without carrying this pipeline
//! along.
//!
//! The schema skeleton (`then` → `then` → `wait`) is IDENTICAL to
//! `computer_control`'s on purpose: the model learns the pipeline shape
//! once and carries it across both tools — only each tool's particular
//! step kind differs (semantic actions here, pointer actions there).
//!
//! FOCUS CAVEAT: a semantic `press` focuses the target on many toolkits
//! but NOT reliably on every one — the one mechanism that always moves OS
//! keyboard focus is a real click, which is `computer_control`'s job. If
//! chained typing keeps missing the field, click the field element there
//! instead.
//!
//! Cancellation caveat: the blocking actions run on tokio's blocking pool
//! and CANNOT be interrupted by dropping the future. If the caller aborts
//! an `act` that is still auto-waiting, the underlying `spawn_blocking`
//! task keeps running and the action may still fire once the element
//! appears. Callers that need hard cancellation must gate the tool call
//! itself (the harness-level authorization), not rely on future drops.
use super::keyboard::{self, MAX_WAIT_MS};
use super::touch;
use super::types::{ActOutput, ComputerAct};

/// Maximum number of steps in a `then` pipeline (the root step included) —
/// the same cap `computer_control` uses, so the two pipelines behave alike.
pub const ACT_CHAIN_MAX_DEPTH: usize = 8;

/// Run a semantic pipeline on an accessibility-tree element: actions,
/// keyboard steps and waits chained via `then`, executed sequentially in
/// ONE call with `&&` semantics.
///
/// # Errors
///
/// Returns `Err` (before any side effect) for an invalid step anywhere in
/// the chain: a semantic step without a target, keyboard/wait fields on a
/// semantic step (or vice versa), `key` with `text`, `text` with `held`,
/// an unknown key/modifier name, a missing `value`/`numeric_value`/`range`
/// payload, a wait that is 0 or over the cap, or a chain deeper than
/// [`ACT_CHAIN_MAX_DEPTH`]. Returns `Err` mid-chain (earlier steps stay
/// applied — they were real actions) when the app or a visible+enabled
/// selector match does not appear within the timeout, or the platform
/// rejects the action.
pub async fn act(input: &ComputerAct) -> Result<ActOutput, String> {
    validate_chain(input, ACT_CHAIN_MAX_DEPTH)?;
    // `spawn_blocking` needs 'static — clone the (small) input struct in.
    let owned = input.clone();
    tokio::task::spawn_blocking(move || act_blocking(&owned))
        .await
        .map_err(|e| format!("computer_act: blocking task failed: {e}"))?
}

/// Validate the WHOLE chain up front (shape + per-step rules) before any
/// synthetic action: a malformed step 3 must not leave steps 1-2 applied
/// with an error that reads like an execution failure.
pub(crate) fn validate_chain(step: &ComputerAct, remaining: usize) -> Result<(), String> {
    if remaining == 0 {
        return Err(format!(
            "computer_act: `then` chain exceeds {ACT_CHAIN_MAX_DEPTH} steps"
        ));
    }
    validate_step(step)?;
    if let Some(next) = &step.then {
        validate_chain(next, remaining - 1)?;
    }
    Ok(())
}

/// Validate ONE step. A step is a WAIT step when `wait` is present (it is
/// standalone), a KEYBOARD step when `key`/`text` is present, and otherwise
/// a SEMANTIC step — the field groups are disjoint, and mixing them in one
/// step is a mistake the model should hear about. The kind-specific rules
/// live in the components.
fn validate_step(step: &ComputerAct) -> Result<(), String> {
    const TOOL: &str = "computer_act";

    // Wait step: STANDALONE — it carries no action, so any other field next
    // to it is ambiguous about what the wait applies to.
    if let Some(ms) = step.wait {
        let extra: Vec<&str> = [
            (step.name.is_some(), "`name`"),
            (step.pid.is_some(), "`pid`"),
            (step.surface.is_some(), "`surface`"),
            (step.selector.is_some(), "`selector`"),
            (step.nth.is_some(), "`nth`"),
            (step.action.is_some(), "`action`"),
            (step.value.is_some(), "`value`"),
            (step.numeric_value.is_some(), "`numeric_value`"),
            (step.range.is_some(), "`range`"),
            (step.timeout_ms.is_some(), "`timeout_ms`"),
            (step.key.is_some(), "`key`"),
            (step.text.is_some(), "`text`"),
            (step.held.is_some(), "`held`"),
        ]
        .iter()
        .filter(|(present, _)| *present)
        .map(|(_, name)| *name)
        .collect();
        if !extra.is_empty() {
            return Err(format!(
                "{TOOL}: `wait` is a standalone step — it takes no {}",
                extra.join(", ")
            ));
        }
        if ms == 0 {
            return Err(format!("{TOOL}: `wait` must be >= 1 ms"));
        }
        if ms > MAX_WAIT_MS {
            return Err(format!(
                "{TOOL}: `wait` is capped at {MAX_WAIT_MS} ms per step — \
                 chain several wait steps for longer pauses"
            ));
        }
        return Ok(());
    }

    let key = step.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = step.text.as_deref().filter(|t| !t.is_empty());

    if key.is_some() && text.is_some() {
        return Err(format!("{TOOL}: provide `key` or `text`, not both"));
    }
    if key.is_some() || text.is_some() {
        // Keyboard step: typing goes into whatever holds keyboard focus —
        // semantic targeting here has no meaning and would silently
        // disagree with it.
        for (present, name) in [
            (step.name.is_some(), "`name`"),
            (step.pid.is_some(), "`pid`"),
            (step.surface.is_some(), "`surface`"),
            (step.selector.is_some(), "`selector`"),
            (step.nth.is_some(), "`nth`"),
            (step.action.is_some(), "`action`"),
            (step.value.is_some(), "`value`"),
            (step.numeric_value.is_some(), "`numeric_value`"),
            (step.range.is_some(), "`range`"),
            (step.timeout_ms.is_some(), "`timeout_ms`"),
        ] {
            if present {
                return Err(format!(
                    "{TOOL}: {name} belongs to a semantic-action step — a keyboard step \
                     types into the focused element; act on the target element in an \
                     earlier step, then chain the typing via `then`. If the typed text \
                     keeps missing the field, click the field element with \
                     computer_control instead — a real click is the one mechanism that \
                     reliably moves keyboard focus"
                ));
            }
        }
        if text.is_some() {
            if step.held.is_some() {
                return Err(format!(
                    "{TOOL}: `held` is rejected with `text` ({text:?}) — backends \
                     synthesize case/shift; use `key` with `held` for chords"
                ));
            }
        } else {
            keyboard::parse_key(key.unwrap_or_default(), TOOL)?;
        }
        keyboard::parse_keys(step.held.as_deref().unwrap_or_default(), TOOL)?;
        return Ok(());
    }

    // Semantic step: the kind-specific rules live in the touch component.
    touch::validate(step, TOOL)
}

/// True when walking the chain will need the input simulator — i.e. some
/// step is a keyboard step (semantic actions go through the accessibility
/// tree, waits through `std::thread`, neither touches the simulator).
/// Acquiring the simulator lazily keeps a purely semantic chain working on
/// systems where no input backend exists.
///
/// The recursion must pass THROUGH wait steps, not stop at them: a wait
/// may carry a `then` continuation (the canonical settle-then-type chain),
/// so only the keyboard classification of each step decides — never an
/// early `false` on the wait itself.
pub(crate) fn chain_needs_sim(step: &ComputerAct) -> bool {
    keyboard::is_keyboard_step(step.key.as_deref(), step.text.as_deref())
        || step.then.as_deref().is_some_and(chain_needs_sim)
}

fn act_blocking(input: &ComputerAct) -> Result<ActOutput, String> {
    let sim = if chain_needs_sim(input) {
        Some(keyboard::shared_sim().map_err(|e| format!("computer_act: input backend: {e}"))?)
    } else {
        None
    };
    // Walk the chain iteratively (&&-semantics): one step at a time, each
    // only reached if the previous succeeded; the first failure aborts with
    // the exact step and everything that completed.
    let mut report = String::new();
    let mut step = input;
    let mut index = 1usize;
    loop {
        let sent = run_step(sim.as_ref(), step).map_err(|e| {
            if report.is_empty() {
                format!("computer_act: step {index}: {e}")
            } else {
                format!("computer_act: step {index}: {e} — completed: {report}")
            }
        })?;
        if !report.is_empty() {
            report.push_str(" → ");
        }
        report.push_str(&sent);
        match &step.then {
            Some(next) => {
                step = next;
                index += 1;
            }
            None => return Ok(ActOutput { sent: report }),
        }
    }
}

/// Execute ONE validated step and return its human-facing report.
fn run_step(sim: Option<&xa11y::InputSim>, step: &ComputerAct) -> Result<String, String> {
    // Wait step: a plain pause (validated standalone).
    if let Some(ms) = step.wait {
        return Ok(keyboard::run_wait_step(ms));
    }
    // Classify EXACTLY like validate_step via the SHARED engine helpers.
    let key = step.key.as_deref();
    let text = step.text.as_deref();
    if keyboard::is_keyboard_step(key, text) {
        let sim = sim.expect("chain_needs_sim: a keyboard step guarantees a simulator");
        keyboard::run_keyboard_step(
            sim,
            &keyboard::KeyboardStep {
                key,
                text,
                held: step.held.as_deref(),
            },
            "computer_act",
        )
    } else {
        touch::run_step(step, "computer_act")
    }
}
