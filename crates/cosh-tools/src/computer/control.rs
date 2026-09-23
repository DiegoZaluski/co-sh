//! `computer_control` — pointer AND keyboard as one pipeline tool.
//!
//! Collapses the former `computer_pointer` + `computer_keyboard` pair: one
//! step is EITHER a pointer action (`action` + target, default `click`) OR a
//! keyboard action (`key`/`text`), and steps chain via `then` with shell
//! `&&` semantics — each step runs only if the previous succeeded, the
//! first failure aborts the chain reporting the exact failing step and
//! everything that completed before it.
//!
//! This file is ONLY the pipeline: chain walking, wait steps and step
//! classification. The step KINDS live in their components — pointer
//! dispatch in [`super::mouse`], keyboard dispatch in [`super::keyboard`]
//! — so each can be reused by other tools without carrying this pipeline
//! along.
//!
//! Targeting is PER STEP (`app`/`pid`/`surface` + `selector` on every
//! step), which is
//! what dissolves the focus problem the two old tools wrestled with: a real
//! click on an element is the one mechanism that moves OS keyboard focus on
//! every toolkit, so the canonical pipeline is click the field → then type.
//! No focus synthesis, no gates — the click that selects IS the focus.
//! Keystrokes go through the shared input simulator (see
//! [`super::shared_input_sim`]); every call is blocking, so it runs on
//! tokio's blocking pool.
use super::keyboard::{self, MAX_WAIT_MS};
use super::mouse;
use super::types::{ComputerControl, ControlOutput};

/// Maximum number of steps in a `then` pipeline (the root step included);
/// 8 covers every realistic sequence (click → type → enter, drag → key, …).
pub const CONTROL_CHAIN_MAX_DEPTH: usize = 8;

/// Run a desktop-control pipeline: pointer and keyboard steps chained via
/// `then`, executed sequentially in ONE call with `&&` semantics — each
/// step runs only if the previous succeeded, and the first failure aborts
/// with the exact failing step and the report of what completed.
///
/// # Errors
///
/// Returns `Err` (before any side effect) for an invalid step anywhere in
/// the chain: a step with both or neither action kind, pointer fields on a
/// keyboard step, `key` with `text`, `text` with `held`, unknown key or
/// modifier names, an invalid pointer target combination, or a chain
/// deeper than [`CONTROL_CHAIN_MAX_DEPTH`]. Returns `Err` mid-chain
/// (earlier steps stay applied — they were real events) when an
/// app/selector does not resolve or the platform input backend is
/// unavailable.
pub async fn control(input: &ComputerControl) -> Result<ControlOutput, String> {
    validate_chain(input, CONTROL_CHAIN_MAX_DEPTH)?;
    // `spawn_blocking` needs 'static — clone the (small) input struct in.
    let owned = input.clone();
    tokio::task::spawn_blocking(move || control_blocking(&owned))
        .await
        .map_err(|e| format!("computer_control: blocking task failed: {e}"))?
}

/// Validate the WHOLE chain up front (shape + per-step rules) before any
/// synthetic event: a malformed step 3 must not leave steps 1-2 applied
/// with an error that reads like an execution failure.
pub(crate) fn validate_chain(step: &ComputerControl, remaining: usize) -> Result<(), String> {
    if remaining == 0 {
        return Err(format!(
            "computer_control: `then` chain exceeds {CONTROL_CHAIN_MAX_DEPTH} steps"
        ));
    }
    validate_step(step)?;
    if let Some(next) = &step.then {
        validate_chain(next, remaining - 1)?;
    }
    Ok(())
}

/// Validate ONE step. A step is a KEYBOARD step when `key` or `text` is
/// present, otherwise a POINTER step — the two field groups are disjoint,
/// and mixing them in one step is a mistake the model should hear about.
/// The kind-specific rules live in the components.
fn validate_step(step: &ComputerControl) -> Result<(), String> {
    // Wait step: STANDALONE — it carries no action, so any other field next
    // to it is ambiguous about what the wait applies to. Cap the duration:
    // the pipeline stays one tool call, and a wait is a pause for the app
    // to catch up, not a sleep primitive.
    if let Some(ms) = step.wait {
        let extra: Vec<&str> = [
            (step.action.is_some(), "`action`"),
            (step.x.is_some(), "`x`"),
            (step.y.is_some(), "`y`"),
            (step.button.is_some(), "`button`"),
            (step.dx.is_some(), "`dx`"),
            (step.dy.is_some(), "`dy`"),
            (step.app.is_some(), "`app`"),
            (step.pid.is_some(), "`pid`"),
            (step.surface.is_some(), "`surface`"),
            (step.selector.is_some(), "`selector`"),
            (step.nth.is_some(), "`nth`"),
            (step.anchor.is_some(), "`anchor`"),
            (step.count.is_some(), "`count`"),
            (step.held.is_some(), "`held`"),
            (step.x2.is_some(), "`x2`"),
            (step.y2.is_some(), "`y2`"),
            (step.to_selector.is_some(), "`to_selector`"),
            (step.duration_ms.is_some(), "`duration_ms`"),
            (step.key.is_some(), "`key`"),
            (step.text.is_some(), "`text`"),
        ]
        .iter()
        .filter(|(present, _)| *present)
        .map(|(_, name)| *name)
        .collect();
        if !extra.is_empty() {
            return Err(format!(
                "computer_control: `wait` is a standalone step — it takes no {}",
                extra.join(", ")
            ));
        }
        if ms == 0 {
            return Err("computer_control: `wait` must be >= 1 ms".into());
        }
        if ms > MAX_WAIT_MS {
            return Err(format!(
                "computer_control: `wait` is capped at {MAX_WAIT_MS} ms per step — \
                 chain several wait steps for longer pauses"
            ));
        }
        return Ok(());
    }

    let key = step.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = step.text.as_deref().filter(|t| !t.is_empty());

    if key.is_some() && text.is_some() {
        return Err("computer_control: provide `key` or `text`, not both".into());
    }
    if key.is_some() || text.is_some() {
        // Keyboard step: typing goes into the element an EARLIER step's
        // element click focused — targeting here has no meaning and would
        // silently disagree with it.
        for (present, name) in [
            (step.action.is_some(), "`action`"),
            (step.x.is_some(), "`x`"),
            (step.y.is_some(), "`y`"),
            (step.button.is_some(), "`button`"),
            (step.dx.is_some(), "`dx`"),
            (step.dy.is_some(), "`dy`"),
            (step.anchor.is_some(), "`anchor`"),
            (step.count.is_some(), "`count`"),
            (step.x2.is_some(), "`x2`"),
            (step.y2.is_some(), "`y2`"),
            (step.to_selector.is_some(), "`to_selector`"),
            (step.duration_ms.is_some(), "`duration_ms`"),
            (step.app.is_some(), "`app`"),
            (step.pid.is_some(), "`pid`"),
            (step.surface.is_some(), "`surface`"),
            (step.selector.is_some(), "`selector`"),
            (step.nth.is_some(), "`nth`"),
        ] {
            if present {
                return Err(format!(
                    "computer_control: {name} belongs to a pointer step — a keyboard step \
                     types into the focused element; click the target element in an \
                     earlier step (or coordinates), then chain the typing via `then`"
                ));
            }
        }
        if let Some(text) = text {
            if step.held.is_some() {
                return Err(format!(
                    "computer_control: `held` is rejected with `text` ({text:?}) — backends \
                     synthesize case/shift; use `key` with `held` for chords"
                ));
            }
        } else {
            keyboard::parse_key(key.unwrap_or_default(), "computer_control")?;
        }
        keyboard::parse_keys(step.held.as_deref().unwrap_or_default(), "computer_control")?;
        return Ok(());
    }

    // Pointer step: the kind-specific rules live in the mouse component.
    mouse::validate(step, "computer_control")
}

fn control_blocking(input: &ComputerControl) -> Result<ControlOutput, String> {
    let sim =
        keyboard::shared_sim().map_err(|e| format!("computer_control: input backend: {e}"))?;
    // Walk the chain iteratively (&&-semantics): one step at a time, each
    // only reached if the previous succeeded; the first failure aborts with
    // the exact step and everything that completed.
    let mut report = String::new();
    let mut step = input;
    let mut index = 1usize;
    loop {
        let sent = run_step(&sim, step).map_err(|e| {
            if report.is_empty() {
                format!("computer_control: step {index}: {e}")
            } else {
                format!("computer_control: step {index}: {e} — completed: {report}")
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
            None => return Ok(ControlOutput { sent: report }),
        }
    }
}

/// Execute ONE validated step and return its human-facing report.
fn run_step(sim: &xa11y::InputSim, step: &ComputerControl) -> Result<String, String> {
    // Wait step: a plain pause (validated standalone) — gives the app time
    // to open a dialog or create an ephemeral field before the next step.
    if let Some(ms) = step.wait {
        return Ok(keyboard::run_wait_step(ms));
    }
    // Classify EXACTLY like validate_step via the SHARED engine helpers, so
    // validation and execution can never disagree about the step's kind.
    let key = step.key.as_deref();
    let text = step.text.as_deref();
    if keyboard::is_keyboard_step(key, text) {
        keyboard::run_keyboard_step(
            sim,
            &keyboard::KeyboardStep {
                key,
                text,
                held: step.held.as_deref(),
            },
            "computer_control",
        )
    } else {
        mouse::run_step(sim, step, "computer_control")
    }
}
