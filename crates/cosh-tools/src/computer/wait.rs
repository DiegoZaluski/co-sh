//! `computer_wait` — block until a condition is met, or time out with a
//! diagnosis.
//!
//! This is a standalone OBSERVATION tool (like [`super::apps`] and
//! [`super::snapshot`]): it sends no input and moves nothing, so it is
//! allowed in every mode including Ask. It replaces poll-loops (snapshot →
//! check → snapshot …, each a full round trip) with one blocking call:
//!
//! - ELEMENT conditions (`selector` + `state`/`value`/`value_contains`):
//!   wait until a selector reaches a state or its value matches, failing
//!   with the condition AND the last observed state so the model can
//!   decide the next move without another probe.
//! - EVENT conditions (`event` + app scope only): subscribe to the
//!   platform's accessibility event stream for that app and block until
//!   the first matching event arrives — "wait until a window opens" or
//!   "wait until focus moves inside the app" answerable in ONE call,
//!   without any polling at all.
//!
//! Every xa11y call is blocking (platform accessibility APIs are
//! synchronous), so it runs on tokio's blocking pool like every other
//! tool in this module.
use std::time::{Duration, Instant};

use xa11y::{App, AppExt, ElementState, EventKind, Locator, StateFlag};

use super::types::{
    ComputerWait, WaitEvent, WaitObservation, WaitOutput, WaitResult, WaitState, WaitedEvent,
    WaitedEventTarget,
};

/// Default wait before giving up: long enough for dialogs to open and
/// spinners to finish, short enough that a stuck call wastes one round
/// trip, not a minute.
pub const DEFAULT_WAIT_MS: u64 = 10_000;

/// Hard cap on a single wait: a stuck condition must not pin the agent
/// loop (or the blocking pool) for minutes. Chain calls for longer waits.
pub const MAX_WAIT_MS: u64 = 60_000;

/// Input validation for `computer_wait`, per mode:
///
/// - EVENT mode (`event`): exactly one app scope, NO element fields, cap.
/// - VALUE mode (`value`/`value_contains`): `selector` required, `state`
///   excluded, exactly one app scope, a 1-based `nth`, cap.
/// - STATE mode (default): `selector` required, the same shared checks.
///
/// # Errors
///
/// Returns `Err` for a missing/blank `selector` (element modes), `name`
/// and `pid` together, no app scope at all, an event wait carrying
/// element fields, `value` with `value_contains`, a value wait with
/// `state`, a 0-based `nth`, or a `timeout_ms` above [`MAX_WAIT_MS`].
pub fn validate(input: &ComputerWait) -> Result<(), String> {
    const TOOL: &str = "computer_wait";
    let require_selector = |tool_field: &str| -> Result<(), String> {
        if input
            .selector
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            return Err(format!(
                "{TOOL}: {tool_field} requires `selector` — the element to watch, \
                 e.g. progress_bar[name='Exporting…']"
            ));
        }
        Ok(())
    };

    if input.event.is_some() {
        // Event mode: app-scoped only — every element field is at best
        // ambiguous, at worst silently meaningless.
        let extra: Vec<&str> = [
            (input.selector.is_some(), "`selector`"),
            (input.nth.is_some(), "`nth`"),
            (input.state.is_some(), "`state`"),
            (input.value.is_some(), "`value`"),
            (input.value_contains.is_some(), "`value_contains`"),
        ]
        .iter()
        .filter(|(present, _)| *present)
        .map(|(_, name)| *name)
        .collect();
        if !extra.is_empty() {
            return Err(format!(
                "{TOOL}: an `event` wait is app-scoped — it takes no {}",
                extra.join(", ")
            ));
        }
    } else {
        if input.value.is_some() && input.value_contains.is_some() {
            return Err(format!(
                "{TOOL}: provide `value` or `value_contains`, not both"
            ));
        }
        let value_condition = input.value.is_some() || input.value_contains.is_some();
        if value_condition {
            require_selector("`value`/`value_contains`")?;
            if input.state.is_some() {
                return Err(format!(
                    "{TOOL}: `value`/`value_contains` and `state` cannot share a call — \
                     wait for the value OR the state (call twice if you need both)"
                ));
            }
        } else {
            require_selector("`state`")?;
        }
    }

    let has_name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if has_name && input.pid.is_some() {
        return Err(format!("{TOOL}: provide `name` or `pid`, not both"));
    }
    if !has_name && input.pid.is_none() {
        return Err(format!("{TOOL}: provide `name` or `pid`"));
    }
    if input.nth == Some(0) {
        return Err(format!("{TOOL}: `nth` is 1-based; use 1 for the first match"));
    }
    if input.timeout_ms.is_some_and(|ms| ms > MAX_WAIT_MS) {
        return Err(format!(
            "{TOOL}: `timeout_ms` is capped at {MAX_WAIT_MS} ms per call — \
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
pub async fn wait(input: &ComputerWait) -> Result<WaitResult, String> {
    let input = input.clone();
    tokio::task::spawn_blocking(move || wait_blocking(&input))
        .await
        .map_err(|e| format!("computer_wait: blocking task failed: {e}"))?
}

fn wait_blocking(input: &ComputerWait) -> Result<WaitResult, String> {
    const TOOL: &str = "computer_wait";
    validate(input)?;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_WAIT_MS));
    // The wall clock starts HERE, not at the poll loop: the caller asked
    // "answer within timeout_ms", so the answer (or the timeout error)
    // must reflect the whole call, app resolution included.
    let started = Instant::now();

    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // The app lookup SHARES the call's budget instead of getting a fresh
    // one: bounded to the same 3 s slice snapshot uses for "app surfaces",
    // it can never eat the budget the condition needs.
    let app_timeout = timeout.min(Duration::from_millis(super::snapshot::DEFAULT_TIMEOUT_MS));
    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(&name, app_timeout),
        (None, Some(pid)) => App::by_pid(pid, app_timeout),
        _ => unreachable!("validated: computer_wait carries exactly one app scope"),
    }
    .map_err(|e| super::errors::render_app_miss(TOOL, &e))?;

    // Event mode: subscribe BEFORE the element machinery — the
    // subscription only carries events RECEIVED from now on, so it must
    // be up while the budget runs.
    if let Some(event) = input.event {
        return wait_for_event(app, event, timeout, started);
    }

    let selector = input
        .selector
        .as_deref()
        .expect("validated: computer_wait carries a selector");
    let locator = app.locator(selector.trim()).nth(input.nth.unwrap_or(1));
    // Value mode: a predicate over the element's value — exact or
    // substring. xa11y's WAIT semantics handle the matching polling; the
    // success payload carries the fresh element the condition was met on.
    if let Some(path) = &input.value {
        let remaining = timeout.saturating_sub(started.elapsed());
        return wait_value(locator, remaining, started, move |value| {
            value == path.as_str()
        })
        .map(WaitResult::State);
    }
    if let Some(needle) = &input.value_contains {
        let remaining = timeout.saturating_sub(started.elapsed());
        return wait_value(locator, remaining, started, move |value| {
            value.contains(needle.as_str())
        })
        .map(WaitResult::State);
    }

    let state = wait_state(input.state.unwrap_or_default());
    // The element wait gets whatever budget the app lookup left — the
    // TOTAL stays within the caller's timeout_ms (xa11y's poll loop
    // evaluates the predicate at least once even at Duration::ZERO, and
    // overshoots by at most one ~100 ms poll).
    let remaining = timeout.saturating_sub(started.elapsed());
    wait_on_locator(&locator, state, remaining, started).map(WaitResult::State)
}

/// Block until the app emits an event of the requested kind, then report
/// the matched event (kind wire name, source app, target snapshot).
fn wait_for_event(
    app: App,
    event: WaitEvent,
    timeout: Duration,
    started: Instant,
) -> Result<WaitResult, String> {
    const TOOL: &str = "computer_wait";
    let wanted = event_kind(event);
    let subscription = app
        .subscribe()
        .map_err(|e| super::errors::render(TOOL, "subscribe to events", &e))?;
    let remaining = timeout.saturating_sub(started.elapsed());
    let arrived = subscription
        .wait_for(|e| event_matches(&wanted, &e.kind), remaining)
        .map_err(|e| super::errors::render(TOOL, "wait for event", &e))?;
    Ok(WaitResult::Event(WaitedEvent {
        kind: event_kind_wire(&arrived.kind),
        app: arrived.app_name,
        pid: Some(arrived.app_pid),
        element: arrived.target.as_ref().map(|data| WaitedEventTarget {
            role: data.role.to_snake_case().to_string(),
            name: data.name.clone(),
            value: data.value.clone(),
        }),
    }))
}

/// Whether a delivered event counts as the kind we are waiting for.
///
/// `EventKind::StateChanged` carries a payload (`flag`, `value`), so a
/// plain equality check would only match ONE specific change (say,
/// "enabled became false") — a `state_changed` wait must match ANY state
/// change. Every other kind is compared exactly.
pub(crate) fn event_matches(wanted: &EventKind, seen: &EventKind) -> bool {
    match (wanted, seen) {
        (EventKind::StateChanged { .. }, EventKind::StateChanged { .. }) => true,
        (wanted, seen) => wanted == seen,
    }
}

/// Map the wire event enum onto xa11y's vocabulary — total, so a missed
/// arm fails compilation rather than waiting for the wrong event. The
/// `StateChanged` payload is a placeholder; [`event_matches`] ignores it
/// (any state change counts).
pub(crate) fn event_kind(event: WaitEvent) -> EventKind {
    match event {
        WaitEvent::WindowOpened => EventKind::WindowOpened,
        WaitEvent::WindowClosed => EventKind::WindowClosed,
        WaitEvent::WindowActivated => EventKind::WindowActivated,
        WaitEvent::WindowDeactivated => EventKind::WindowDeactivated,
        WaitEvent::FocusChanged => EventKind::FocusChanged,
        WaitEvent::ValueChanged => EventKind::ValueChanged,
        WaitEvent::TextChanged => EventKind::TextChanged,
        WaitEvent::NameChanged => EventKind::NameChanged,
        WaitEvent::StateChanged => EventKind::StateChanged {
            flag: StateFlag::Enabled,
            value: true,
        },
        WaitEvent::StructureChanged => EventKind::StructureChanged,
        WaitEvent::SelectionChanged => EventKind::SelectionChanged,
        WaitEvent::MenuOpened => EventKind::MenuOpened,
        WaitEvent::MenuClosed => EventKind::MenuClosed,
        WaitEvent::Announcement => EventKind::Announcement,
    }
}

/// Wire-facing name of a delivered event kind — the schema vocabulary the
/// caller sent and expects echoed back.
pub(crate) fn event_kind_wire(kind: &EventKind) -> String {
    match kind {
        EventKind::FocusChanged => "focus_changed".into(),
        EventKind::ValueChanged => "value_changed".into(),
        EventKind::NameChanged => "name_changed".into(),
        EventKind::StateChanged { .. } => "state_changed".into(),
        EventKind::StructureChanged => "structure_changed".into(),
        EventKind::WindowOpened => "window_opened".into(),
        EventKind::WindowClosed => "window_closed".into(),
        EventKind::WindowActivated => "window_activated".into(),
        EventKind::WindowDeactivated => "window_deactivated".into(),
        EventKind::SelectionChanged => "selection_changed".into(),
        EventKind::MenuOpened => "menu_opened".into(),
        EventKind::MenuClosed => "menu_closed".into(),
        EventKind::TextChanged => "text_changed".into(),
        EventKind::Announcement => "announcement".into(),
        other => format!("{other:?}"),
    }
}

/// Block until the locator's element's value satisfies the predicate.
///
/// `locator.wait_until` polls the SAME 100 ms cadence the state waits
/// use; its predicate sees `None` until the selector matches, which
/// simply means "keep waiting".
fn wait_value(
    locator: Locator,
    timeout: Duration,
    started: Instant,
    predicate: impl Fn(&str) -> bool,
) -> Result<WaitOutput, String> {
    const TOOL: &str = "computer_wait";
    let wrapped = move |data: Option<&xa11y::ElementData>| {
        data.as_ref()
            .and_then(|d| d.value.as_deref())
            .is_some_and(&predicate)
    };
    match locator.wait_until(wrapped, timeout) {
        Ok(maybe) => {
            let observed = match maybe.as_ref().map(|e| e.data()) {
                Some(data) => observation(&data.states, true),
                None => WaitObservation {
                    attached: false,
                    visible: None,
                    enabled: None,
                    focused: None,
                },
            };
            Ok(WaitOutput {
                met: true,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                observed,
            })
        }
        Err(e) => Err(super::errors::render(
            TOOL,
            "wait for element value",
            &e,
        )),
    }
}

/// Map the wire enum onto xa11y's state vocabulary.
pub(crate) fn wait_state(state: WaitState) -> ElementState {
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
pub(crate) fn wait_on_locator(
    locator: &Locator,
    state: ElementState,
    timeout: Duration,
    started: Instant,
) -> Result<WaitOutput, String> {
    match locator.wait_for_state(state, timeout) {
        Ok(Some(element)) => Ok(WaitOutput {
            met: true,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
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

fn observation(states: &xa11y::StateSet, attached: bool) -> WaitObservation {
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
