//! Demonstrate `computer_wait` — block until an element in a target app
//! reaches a state (`visible`, `enabled`, `focused`, `detached`, …), or
//! time out with a Diagnosis of the last observed state.
//!
//! The problem it solves: without a wait, the model retries a snapshot in
//! a tight loop or — worse — acts on a stale picture. With it, the model
//! blocks ON the condition and, if the condition never arrives, the
//! timeout error TEACHES what was actually observed (near-miss candidates
//! included) instead of just failing.
//!
//! What runs offline here: input validation and the wait-state vocabulary.
//! The poll loop itself needs an application to watch — dispatch requires
//! a desktop session.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example computer-wait
//! ```

use cosh_tools::computer::types::{ComputerWait, WaitState};
use cosh_tools::computer::wait::validate;

fn main() {
    // ── The canonical use: block until the busy indicator goes away ──
    //
    // `detached` tolerates the element NEVER having existed — a spinner
    // that already finished is a satisfied wait, not an error.
    let spinner_gone = ComputerWait {
        name: Some("Reports".into()),
        selector: Some("progress_bar[name='Exporting…']".into()),
        state: Some(WaitState::Detached),
        timeout_ms: Some(30_000),
        ..Default::default()
    };
    assert!(validate(&spinner_gone).is_ok());
    println!("detached wait: valid — returns WaitOutput {{ met, elapsed_ms, observed }}");

    // ── Wait for a dialog's OK button to become clickable ──
    //
    // `enabled` is satisfied only by an element that exists AND reports
    // enabled — the guard before pressing a control that renders disabled
    // while a form is incomplete.
    let ok_clickable = ComputerWait {
        pid: Some(4242),
        selector: Some("button[name='OK']".into()),
        state: Some(WaitState::Enabled),
        // Default 10 000 ms; hard cap 60 000 — a stuck call cannot pin
        // the session for minutes.
        timeout_ms: Some(15_000),
        ..Default::default()
    };
    assert!(validate(&ok_clickable).is_ok());

    // ── Validation errors, offline ──
    //
    // Exactly one app scope (`name` OR `pid`) and a selector are required:
    let scopeless = ComputerWait::default();
    if let Err(e) = validate(&scopeless) {
        println!("missing target rejected: {e}");
    }

    let both_scopes = ComputerWait {
        name: Some("Reports".into()),
        pid: Some(4242),
        selector: Some("button".into()),
        ..Default::default()
    };
    if let Err(e) = validate(&both_scopes) {
        println!("double scope rejected: {e}");
    }

    // The timeout cap is validated up front (60 000 ms):
    let too_patient = ComputerWait {
        name: Some("Reports".into()),
        selector: Some("button".into()),
        timeout_ms: Some(61_000),
        ..Default::default()
    };
    if let Err(e) = validate(&too_patient) {
        println!("over-cap timeout rejected: {e}");
    }

    // ── The full state vocabulary ──
    //
    // attached | detached | visible (default) | hidden | enabled | disabled
    //         | focused | unfocused — the same tokens computer_snapshot
    // reports as state flags, so a snapshot read leads directly to a wait.
    let states = [
        ("attached", WaitState::Attached),
        ("detached", WaitState::Detached),
        ("visible", WaitState::Visible),
        ("hidden", WaitState::Hidden),
        ("enabled", WaitState::Enabled),
        ("disabled", WaitState::Disabled),
        ("focused", WaitState::Focused),
        ("unfocused", WaitState::Unfocused),
    ];
    for (spelling, state) in states {
        let input = ComputerWait {
            name: Some("Reports".into()),
            selector: Some("button".into()),
            state: Some(state),
            ..Default::default()
        };
        assert!(validate(&input).is_ok(), "{spelling} must be valid");
        println!("{spelling}: ok");
    }

    // Full dispatch (needs a desktop session). On timeout the error carries
    // the Diagnosis: the condition waited for, the selector, what was LAST
    // observed ("matched button \"OK\" (enabled=false)" or "selector never
    // matched") — the model reads it and re-plans instead of retrying blind:
    // computer::wait(&ok_clickable).await
}
