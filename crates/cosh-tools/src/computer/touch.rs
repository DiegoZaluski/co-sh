//! `computer_touch` — perform an action on an element found via the
//! accessibility tree (no screen coordinates involved).
//!
//! Actions go through [`xa11y::Locator`]'s auto-waiting methods: unlike
//! `computer_snapshot`/`computer_screenshot` (fail-fast resolution), most
//! actions WAIT for the element to be visible and enabled within
//! `timeout_ms`, because the point of a touch is usually to interact with
//! an interface that is still settling. (Exception: `scroll_into_view`
//! intentionally waits only for the element to EXIST — its purpose is to
//! bring a not-yet-visible element into view.)
//!
//! Cancellation caveat: the blocking action runs on tokio's blocking pool
//! and CANNOT be interrupted by dropping the future. If the caller aborts
//! a `touch` that is still auto-waiting, the underlying `spawn_blocking`
//! task keeps running and the action may still fire once the element
//! appears. Callers that need hard cancellation must gate the tool call
//! itself (the harness-level authorization), not rely on future drops.
use std::time::Duration;

use xa11y::{App, AppExt};

use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{TouchAction, TouchOutput, ComputerTouch};

/// Perform an action on the element matched by `selector`.
///
/// Resolves the app by `name` or `pid` (exactly one required), matches
/// `selector` (auto-waiting for a visible + enabled match), and invokes
/// the requested accessibility action. Value-carrying actions validate
/// their payload up front (`set_value`/`type_text`/`perform_action` need
/// `value`, `set_numeric_value` needs `numeric_value`, `select_text` needs
/// a two-element `range`).
///
/// # Errors
///
/// Returns `Err` for invalid input (name and pid both set or both missing,
/// `nth` of 0, a missing `value`/`numeric_value`/`range` payload), when the
/// app or a visible+enabled selector match does not appear within the
/// timeout, or when the platform rejects the action (e.g. `set_value` on a
/// non-editable element).
pub async fn touch(input: &ComputerTouch) -> Result<TouchOutput, String> {
    if input.selector.trim().is_empty() {
        return Err("computer_touch: `selector` is required".into());
    }
    let has_name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();
    if has_name && input.pid.is_some() {
        return Err("computer_touch: provide `name` or `pid`, not both".into());
    }
    if !has_name && input.pid.is_none() {
        return Err("computer_touch: provide `name` or `pid`".into());
    }
    if input.nth == Some(0) {
        return Err("computer_touch: `nth` is 1-based; use 1 for the first match".into());
    }

    // Payload validation BEFORE spawning, so a malformed call fails fast
    // instead of auto-waiting for an element that was never the problem.
    match input.action.unwrap_or_default() {
        TouchAction::SetValue | TouchAction::TypeText | TouchAction::PerformAction
            if input.value.is_none() =>
        {
            return Err(format!(
                "computer_touch: action `{}` requires `value`",
                action_name(input.action.unwrap_or_default())
            ));
        }
        TouchAction::SetNumericValue if input.numeric_value.is_none() => {
            return Err("computer_touch: action `set_numeric_value` requires `numeric_value`".into());
        }
        TouchAction::SelectText => {
            let Some(range) = &input.range else {
                return Err("computer_touch: action `select_text` requires `range` [start, end]".into());
            };
            if range.len() != 2 {
                return Err(format!(
                    "computer_touch: `range` takes exactly 2 numbers [start, end], got {}",
                    range.len()
                ));
            }
            if range[0] > range[1] {
                return Err(format!(
                    "computer_touch: `range` start ({}) must be <= end ({})",
                    range[0], range[1]
                ));
            }
        }
        _ => {}
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || touch_blocking(&input))
        .await
        .map_err(|e| format!("computer_touch: blocking task failed: {e}"))?
}

fn touch_blocking(input: &ComputerTouch) -> Result<TouchOutput, String> {
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout),
        (None, Some(pid)) => App::by_pid(pid, timeout),
        _ => unreachable!("validated in touch"),
    }
    .map_err(|e| format!("computer_touch: resolve application: {e}"))?;

    let nth = input.nth.unwrap_or(1);
    let locator = app
        .locator(input.selector.trim())
        .nth(nth)
        .with_timeout(timeout);
    let action = input.action.unwrap_or_default();

    let result = match action {
        TouchAction::Press => locator.press(),
        TouchAction::Focus => locator.focus(),
        TouchAction::Blur => locator.blur(),
        TouchAction::Toggle => locator.toggle(),
        TouchAction::Select => locator.select(),
        TouchAction::Expand => locator.expand(),
        TouchAction::Collapse => locator.collapse(),
        TouchAction::ShowMenu => locator.show_menu(),
        TouchAction::Increment => locator.increment(),
        TouchAction::Decrement => locator.decrement(),
        TouchAction::ScrollIntoView => locator.scroll_into_view(),
        TouchAction::SetValue => {
            locator.set_value(input.value.as_deref().unwrap_or_default())
        }
        TouchAction::SetNumericValue => {
            locator.set_numeric_value(input.numeric_value.unwrap_or_default())
        }
        TouchAction::SelectText => {
            let range = input.range.as_deref().unwrap_or_default();
            locator.select_text(range.first().copied().unwrap_or(0), range.get(1).copied().unwrap_or(0))
        }
        TouchAction::TypeText => locator.type_text(input.value.as_deref().unwrap_or_default()),
        TouchAction::PerformAction => {
            locator.perform_action(input.value.as_deref().unwrap_or_default())
        }
    };
    result.map_err(|e| format!("computer_touch: {}: {e}", action_name(action)))?;

    Ok(TouchOutput {
        app: app.name.clone(),
        pid: app.pid,
        action,
    })
}

/// Wire-facing name of an action (matches the `snake_case` serde renaming).
fn action_name(action: TouchAction) -> &'static str {
    match action {
        TouchAction::Press => "press",
        TouchAction::Focus => "focus",
        TouchAction::Blur => "blur",
        TouchAction::Toggle => "toggle",
        TouchAction::Select => "select",
        TouchAction::Expand => "expand",
        TouchAction::Collapse => "collapse",
        TouchAction::ShowMenu => "show_menu",
        TouchAction::Increment => "increment",
        TouchAction::Decrement => "decrement",
        TouchAction::ScrollIntoView => "scroll_into_view",
        TouchAction::SetValue => "set_value",
        TouchAction::SetNumericValue => "set_numeric_value",
        TouchAction::SelectText => "select_text",
        TouchAction::TypeText => "type_text",
        TouchAction::PerformAction => "perform_action",
    }
}
