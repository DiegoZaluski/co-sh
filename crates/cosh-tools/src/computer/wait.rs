//! `computer_wait` — block until an element reaches a state, or time out
//! with a diagnosis.
//!
//! This is a standalone OBSERVATION tool (like [`super::apps`] and
//! [`super::snapshot`]): it sends no input and moves nothing, so it is
//! allowed in every mode including Ask. It replaces poll-loops (snapshot →
//! check → snapshot …, each a full round trip) with one blocking call:
//! wait until a selector reaches a state, or fail with the condition AND
//! the last observed state so the model can decide the next move without
//! another probe.
//!
//! Every xa11y call is blocking (platform accessibility APIs are
//! synchronous), so it runs on tokio's blocking pool like every other
//! tool in this module.
use std::time::{Duration, Instant};

use xa11y::{App, AppExt, ElementState, Locator};

use super::types::{ComputerWait, WaitObservation, WaitOutput, WaitState};

/// Default wait before giving up: long enough for dialogs to open and
/// spinners to finish, short enough that a stuck call wastes one round
/// trip, not a minute.
pub const DEFAULT_WAIT_MS: u64 = 10_000;

/// Hard cap on a single wait: a stuck condition must not pin the agent
/// loop (or the blocking pool) for minutes. Chain calls for longer waits.
pub const MAX_WAIT_MS: u64 = 60_000;

/// Input validation for `computer_wait`: an element target (`selector` +
/// exactly one app scope), a 1-based `nth`, and a timeout within the cap.
///
/// # Errors
///
/// Returns `Err` for a missing/blank `selector`, `name` and `pid`
/// together, no app scope at all, a 0-based `nth`, or a `timeout_ms`
/// above [`MAX_WAIT_MS`].
pub fn validate(input: &ComputerWait) -> Result<(), String> {
    if input
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(
            "computer_wait: `selector` is required — the element to watch, \
             e.g. progress_indicator[name='Exporting…']"
                .to_string(),
        );
    }
    let has_name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if has_name && input.pid.is_some() {
        return Err("computer_wait: provide `name` or `pid`, not both".to_string());
    }
    if !has_name && input.pid.is_none() {
        return Err("computer_wait: provide `name` or `pid`".to_string());
    }
    if input.nth == Some(0) {
        return Err("computer_wait: `nth` is 1-based; use 1 for the first match".to_string());
    }
    if input.timeout_ms.is_some_and(|ms| ms > MAX_WAIT_MS) {
        return Err(format!(
            "computer_wait: `timeout_ms` is capped at {MAX_WAIT_MS} ms per call — \
             chain calls for longer waits"
        ));
    }
    Ok(())
}

/// Run `computer_wait` on tokio's blocking pool.
///
/// # Errors
///
/// Returns `Err` for invalid input, an app that never surfaces, a selector
/// that never matches, or — the point of the tool — a condition not met
/// within the timeout. The timeout error embeds xa11y's `Diagnosis` (what
/// was waited for + the last observed state), so the failure message alone
/// answers "why didn't it happen?".
pub async fn wait(input: &ComputerWait) -> Result<WaitOutput, String> {
    let input = input.clone();
    tokio::task::spawn_blocking(move || wait_blocking(&input))
        .await
        .map_err(|e| format!("computer_wait: blocking task failed: {e}"))?
}

fn wait_blocking(input: &ComputerWait) -> Result<WaitOutput, String> {
    validate(input)?;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_WAIT_MS));
    // The wall clock starts HERE, not at the poll loop: the caller asked
    // "answer within timeout_ms", so the answer (or the timeout error)
    // must reflect the whole call, app resolution included.
    let started = Instant::now();

    let name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // The app lookup SHARES the call's budget instead of getting a fresh
    // one (review finding): bounded to the same 3 s slice snapshot uses
    // for "app surfaces", it can never eat the budget the condition
    // needs — but a slow DEE (cold app launch) still gets a fair share
    // when the caller asks for a long wait.
    let app_timeout = timeout.min(Duration::from_millis(super::snapshot::DEFAULT_TIMEOUT_MS));
    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(name, app_timeout),
        (None, Some(pid)) => App::by_pid(pid, app_timeout),
        _ => unreachable!("validated: computer_wait carries exactly one app scope"),
    }
    .map_err(|e| super::errors::render("computer_wait", "resolve application", &e))?;

    let selector = input
        .selector
        .as_deref()
        .expect("validated: computer_wait carries a selector");
    let locator = app
        .locator(selector.trim())
        .nth(input.nth.unwrap_or(1));
    let state = wait_state(input.state.unwrap_or_default());
    // The element wait gets whatever budget the app lookup left — the
    // TOTAL stays within the caller's timeout_ms (xa11y's poll loop
    // evaluates the predicate at least once even at Duration::ZERO, and
    // overshoots by at most one ~100 ms poll).
    let remaining = timeout.saturating_sub(started.elapsed());
    wait_on_locator(&locator, state, remaining, started)
}

/// Map the wire enum onto xa11y's state vocabulary.
fn wait_state(state: WaitState) -> ElementState {
    match state {
        WaitState::Attached => ElementState::Attached,
        WaitState::Detached => ElementState::Detached,
        WaitState::Visible => ElementState::Visible,
        WaitState::Hidden => ElementState::Hidden,
        WaitState::Enabled => ElementState::Enabled,
        WaitState::Disabled => ElementState::Disabled,
        WaitState::Focused => ElementState::Focused,
        WaitState::Unfocused => ElementState::Unfocused,
    }
}

/// Core wait over an already-built locator — the seam the mock-provider
/// tests use (they build a `Locator` directly instead of resolving an
/// app by name against the live desktop).
///
/// `started` is the wall clock of the WHOLE call (app resolution
/// included), so `elapsed_ms` answers "how long did this call take" —
/// the contract the model reasons about when budgeting its next steps.
///
/// On timeout the xa11y error's `Display` already embeds the `Diagnosis`
/// (condition + last observed state); it is passed through verbatim so
/// the model sees exactly what the poll loop last saw.
fn wait_on_locator(
    locator: &Locator,
    state: ElementState,
    timeout: Duration,
    started: Instant,
) -> Result<WaitOutput, String> {
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match locator.wait_for_state(state, timeout) {
        Ok(Some(element)) => Ok(WaitOutput {
            met: true,
            elapsed_ms: elapsed_ms(),
            observed: observation(&element.data().states, true),
        }),
        // An absence condition (detached/hidden) met with nothing in the
        // tree: nothing to observe beyond the fact.
        Ok(None) => Ok(WaitOutput {
            met: true,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            observed: WaitObservation {
                attached: false,
                visible: None,
                enabled: None,
                focused: None,
            },
        }),
        Err(e) => Err(super::errors::render(
            "computer_wait",
            "wait for element state",
            &e,
        )),
    }
}

fn observation(
    states: &xa11y::StateSet,
    attached: bool,
) -> WaitObservation {
    if attached {
        WaitObservation {
            attached: true,
            visible: Some(states.visible),
            enabled: Some(states.enabled),
            focused: Some(states.focused),
        }
    } else {
        WaitObservation {
            attached: false,
            visible: None,
            enabled: None,
            focused: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xa11y::Provider;
    use xa11y::mock::build_provider;

    fn root_locator(selector: &str) -> Locator {
        let provider = build_provider();
        let provider_dyn: std::sync::Arc<dyn Provider> = provider;
        Locator::new(provider_dyn, None, selector)
    }

    #[test]
    fn validation_requires_selector_and_one_app_scope() {
        let input = ComputerWait::default();
        let err = validate(&input).unwrap_err();
        assert!(err.contains("`selector` is required"), "err: {err}");

        let input = ComputerWait {
            selector: Some("button".into()),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("`name` or `pid`"), "err: {err}");

        let input = ComputerWait {
            name: Some("Safari".into()),
            pid: Some(42),
            selector: Some("button".into()),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn validation_rejects_zero_nth_and_overcap_timeout() {
        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            nth: Some(0),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");

        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            timeout_ms: Some(MAX_WAIT_MS + 1),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("capped at"), "err: {err}");

        // Exactly at the cap is fine.
        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            timeout_ms: Some(MAX_WAIT_MS),
            ..Default::default()
        };
        assert!(validate(&input).is_ok());
    }

    #[test]
    fn wait_met_immediately_reports_observed_state() {
        // The mock fixture carries a visible, enabled check_box; the
        // default state (`visible`) is met on the first poll.
        let locator = root_locator("check_box");
        let out = wait_on_locator(
            &locator,
            ElementState::Visible,
            Duration::from_secs(2),
            Instant::now(),
        )
        .expect("visible check_box must meet the condition");
        assert!(out.met);
        assert!(out.elapsed_ms < 2_000);
        assert!(out.observed.attached);
        assert_eq!(out.observed.visible, Some(true));
        assert_eq!(out.observed.enabled, Some(true));
    }

    #[test]
    fn wait_detached_met_without_element() {
        // A selector nothing matches: the ABSENCE condition is met on the
        // first poll — the common "spinner went away" shape.
        let locator = root_locator("button[name='Definitely Not There']");
        let out = wait_on_locator(
            &locator,
            ElementState::Detached,
            Duration::from_secs(2),
            Instant::now(),
        )
        .expect("detached is met when nothing matches");
        assert!(out.met);
        assert!(!out.observed.attached);
        assert_eq!(out.observed.visible, None);
    }

    #[test]
    fn wait_timeout_error_carries_diagnosis() {
        // Attached on a selector that never matches: runs out the (tiny)
        // timeout; the error must name the condition and the last
        // observation, not a bare "timeout".
        let locator = root_locator("button[name='Definitely Not There']");
        let err = wait_on_locator(
            &locator,
            ElementState::Attached,
            Duration::from_millis(150),
            Instant::now(),
        )
        .expect_err("attached on a missing element must time out");
        // Phase 7: the wait path routes through the error renderer — the
        // message must carry the tool prefix, the NEXT-STEP guidance
        // (budget, wait step) and the platform's Diagnosis verbatim, so
        // the model sees exactly what the poll loop last saw.
        assert!(err.starts_with("computer_wait: wait for element state: "), "err: {err}");
        assert!(err.contains("timed out after"), "err: {err}");
        assert!(err.contains("timeout_ms"), "missing budget guidance: {err}");
        assert!(err.contains("`wait` step"), "missing wait-step guidance: {err}");
        assert!(
            err.contains("Attached") || err.contains("attached"),
            "timeout error must name the condition: {err}"
        );
        assert!(
            err.contains("last observed"),
            "diagnosis (last observed) must survive verbatim: {err}"
        );
    }

    #[test]
    fn state_mapping_is_total() {
        // Every wire variant maps onto a distinct xa11y state — a missed
        // arm here would silently wait for the wrong condition.
        let pairs = [
            (WaitState::Attached, ElementState::Attached),
            (WaitState::Detached, ElementState::Detached),
            (WaitState::Visible, ElementState::Visible),
            (WaitState::Hidden, ElementState::Hidden),
            (WaitState::Enabled, ElementState::Enabled),
            (WaitState::Disabled, ElementState::Disabled),
            (WaitState::Focused, ElementState::Focused),
            (WaitState::Unfocused, ElementState::Unfocused),
        ];
        for (wire, native) in pairs {
            assert_eq!(wait_state(wire), native);
        }
    }

    #[test]
    fn mock_provider_type_is_usable_via_selector() {
        // Guards the test seam itself: `build_provider` must yield a
        // provider whose locators resolve. If the fixture topology changes
        // (check_box disappearing), the wait tests above fail HERE with a
        // clearer message.
        let provider = build_provider();
        let provider_dyn: std::sync::Arc<dyn Provider> = provider;
        let locator = Locator::new(provider_dyn, None, "check_box");
        assert!(
            locator.count().map(|n| n >= 1).unwrap_or(false),
            "mock fixture lost its check_box — update the wait tests' fixture assumptions"
        );
    }
}
