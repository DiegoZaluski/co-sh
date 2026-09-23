//! Semantic (accessibility-action) component — validation and dispatch of
//! element-action steps.
//!
//! This is a COMPONENT, not a tool: it knows how to validate and execute
//! ONE semantic action (`press`, `toggle`, `set_value`, …) on the element
//! matched by `selector` in a target app, auto-waiting for the element to
//! become visible and enabled, and nothing else. The pipeline tools
//! (`computer_act`) compose it with the keyboard ([`super::keyboard`]) and
//! wait engines; a future tool that needs semantic element actions can
//! reuse it without carrying the pipelines along.
//!
//! Exception kept from the former `computer_touch`: `scroll_into_view`
//! waits only for the element to EXIST — its purpose is to bring a
//! not-yet-visible element into view, so visibility would deadlock it.
use std::time::Duration;

use xa11y::{App, AppExt};

use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{ActAction, ComputerAct};

/// Semantic-step validation: an element target (`selector` + exactly one
/// app scope) and the action's payload.
///
/// `tool` names the calling pipeline in the error messages, so a future
/// consumer reports its own name instead of a hardcoded one.
///
/// # Errors
///
/// Returns `Err` for a missing/blank `selector`, `name` and `pid`
/// together, no app scope at all, a 0-based `nth`, or a missing payload —
/// `set_value`/`type_text`/`perform_action` need `value`,
/// `set_numeric_value` needs `numeric_value`, `select_text` needs a
/// well-formed `range` (exactly two numbers, start <= end).
pub fn validate(step: &ComputerAct, tool: &str) -> Result<(), String> {
    if step
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(format!(
            "{tool}: a semantic action requires `selector` (the element to act on); \
             use `key`/`text` for typing, or `wait` to pause"
        ));
    }
    super::surface::validate_act(step)?;
    if step.nth == Some(0) {
        return Err(format!("{tool}: `nth` is 1-based; use 1 for the first match"));
    }
    if step.held.is_some() {
        return Err(format!(
            "{tool}: `held` applies to keyboard and pointer steps — a semantic action \
             carries no chord; chain a keyboard step via `then` if you need one"
        ));
    }

    // Payload validation BEFORE dispatching, so a malformed call fails fast
    // instead of auto-waiting for an element that was never the problem.
    match step.action.unwrap_or_default() {
        ActAction::SetValue | ActAction::TypeText | ActAction::PerformAction
            if step.value.is_none() =>
        {
            return Err(format!(
                "{tool}: action `{}` requires `value`",
                action_name(step.action.unwrap_or_default())
            ));
        }
        ActAction::SetNumericValue if step.numeric_value.is_none() => {
            return Err(format!(
                "{tool}: action `set_numeric_value` requires `numeric_value`"
            ));
        }
        ActAction::SelectText => {
            let Some(range) = &step.range else {
                return Err(format!(
                    "{tool}: action `select_text` requires `range` [start, end]"
                ));
            };
            if range.len() != 2 {
                return Err(format!(
                    "{tool}: `range` takes exactly 2 numbers [start, end], got {}",
                    range.len()
                ));
            }
            if range[0] > range[1] {
                return Err(format!(
                    "{tool}: `range` start ({}) must be <= end ({})",
                    range[0], range[1]
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Execute ONE validated semantic action: resolve the target (app or shell
/// surface), match the selector (auto-waiting for visible + enabled),
/// invoke the action. Returns the step's report fragment.
pub fn run_step(step: &ComputerAct, tool: &str) -> Result<String, String> {
    let timeout = Duration::from_millis(step.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let selector = step
        .selector
        .as_deref()
        .expect("validated: semantic step carries a selector");
    let nth = step.nth.unwrap_or(1);
    let action = step.action.unwrap_or_default();

    // Shell-surface targeting: same selector flow, rooted at the surface
    // (validated: exactly one of name/pid/surface).
    if let Some(surface_kind) = step.surface {
        let surface =
            super::surface::resolve(surface_kind, timeout).map_err(|e| format!("{tool}: {e}"))?;
        return run_step_on_surface(step, surface, timeout, tool);
    }
    let name = step.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let app = match (name, step.pid) {
        (Some(name), None) => App::by_name(name, timeout),
        (None, Some(pid)) => App::by_pid(pid, timeout),
        _ => unreachable!("validated: semantic step carries exactly one app scope"),
    }
    .map_err(|e| super::errors::render(tool, "resolve application", &e))?;
    let locator = app.locator(selector.trim()).nth(nth).with_timeout(timeout);
    dispatch_action(&locator, action, step, app.name.clone(), tool)
}

/// Run a validated semantic step against an ALREADY-RESOLVED surface —
/// shared by the production path (singleton provider) and the tests (mock
/// provider handle built via `ShellSurface::by_kind_with`).
pub(crate) fn run_step_on_surface(
    step: &ComputerAct,
    surface: xa11y::ShellSurface,
    timeout: Duration,
    tool: &str,
) -> Result<String, String> {
    let selector = step
        .selector
        .as_deref()
        .expect("validated: semantic step carries a selector");
    let nth = step.nth.unwrap_or(1);
    let action = step.action.unwrap_or_default();
    let locator = surface
        .locator(selector.trim())
        .nth(nth)
        .with_timeout(timeout);
    dispatch_action(&locator, action, step, surface.name.clone(), tool)
}

/// Invoke the action on an already-built locator and format the report
/// fragment — shared by the app and surface paths.
fn dispatch_action(
    locator: &xa11y::Locator,
    action: ActAction,
    step: &ComputerAct,
    target_name: String,
    tool: &str,
) -> Result<String, String> {
    let selector = step
        .selector
        .as_deref()
        .expect("validated: semantic step carries a selector");
    let result = match action {
        ActAction::Press => locator.press(),
        ActAction::Focus => locator.focus(),
        ActAction::Blur => locator.blur(),
        ActAction::Toggle => locator.toggle(),
        ActAction::Select => locator.select(),
        ActAction::Expand => locator.expand(),
        ActAction::Collapse => locator.collapse(),
        ActAction::ShowMenu => locator.show_menu(),
        ActAction::Increment => locator.increment(),
        ActAction::Decrement => locator.decrement(),
        ActAction::ScrollIntoView => locator.scroll_into_view(),
        ActAction::SetValue => locator.set_value(step.value.as_deref().unwrap_or_default()),
        ActAction::SetNumericValue => {
            locator.set_numeric_value(step.numeric_value.unwrap_or_default())
        }
        ActAction::SelectText => {
            let range = step.range.as_deref().unwrap_or_default();
            locator.select_text(
                range.first().copied().unwrap_or(0),
                range.get(1).copied().unwrap_or(0),
            )
        }
        ActAction::TypeText => locator.type_text(step.value.as_deref().unwrap_or_default()),
        ActAction::PerformAction => {
            locator.perform_action(step.value.as_deref().unwrap_or_default())
        }
    };
    result.map_err(|e| {
        super::errors::render(tool, &action_name(action).to_lowercase(), &e)
    })?;

    Ok(format!(
        "{} `{}` ({})",
        action_name(action),
        selector,
        target_name
    ))
}

/// Wire-facing name of an action (matches the `snake_case` serde renaming).
pub fn action_name(action: ActAction) -> &'static str {
    match action {
        ActAction::Press => "press",
        ActAction::Focus => "focus",
        ActAction::Blur => "blur",
        ActAction::Toggle => "toggle",
        ActAction::Select => "select",
        ActAction::Expand => "expand",
        ActAction::Collapse => "collapse",
        ActAction::ShowMenu => "show_menu",
        ActAction::Increment => "increment",
        ActAction::Decrement => "decrement",
        ActAction::ScrollIntoView => "scroll_into_view",
        ActAction::SetValue => "set_value",
        ActAction::SetNumericValue => "set_numeric_value",
        ActAction::SelectText => "select_text",
        ActAction::TypeText => "type_text",
        ActAction::PerformAction => "perform_action",
    }
}
