//! Pointer (mouse) component — validation and dispatch of pointer steps.
//!
//! This is a COMPONENT, not a tool: it knows how to validate and execute
//! ONE pointer step (click/double_click/right_click/move/down/up/scroll/
//! drag) against either desktop coordinates or an accessibility-tree
//! element, and nothing else. The pipeline tools (`computer_control`)
//! compose it with the keyboard ([`super::keyboard`]) and wait engines;
//! any future tool that needs pixel-precise mouse input can reuse it
//! without carrying the pipelines along.
//!
//! Targeting comes in two forms, validated to be mutually exclusive: raw
//! coordinates (`x`/`y`, position-dependent) or the element form
//! (`app`/`pid`/`surface` + `selector`), whose point resolves from the
//! element's CURRENT bounds at dispatch time — never stale.
use std::time::Duration;

use xa11y::{
    Anchor, App, AppExt, ClickOptions, ClickTarget, DragOptions, MouseButton, Point, ScrollDelta,
};

use super::keyboard;
use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{ComputerControl, PointerAction, PointerAnchor};

/// Minimum duration of a `drag` movement — below this the interpolation
/// degenerates to a jump, which drops intermediate hover/enter events.
pub const MIN_DRAG_MS: u64 = 50;

/// Default duration of a `drag` movement, in milliseconds.
pub const DEFAULT_DRAG_MS: u64 = 150;

/// Pointer-step validation: one target form (coordinates XOR element, or
/// neither for `up`), action-specific fields, drag endpoint pairing.
///
/// `tool` names the calling pipeline in the error messages, so a future
/// consumer reports its own name instead of a hardcoded one.
///
/// # Errors
///
/// Returns `Err` for a partial coordinate pair, both target forms at once,
/// a targetless action other than `up`, a leaked element scope
/// (`app`/`pid` without `selector` or vice versa), `name` and `pid`
/// together, a 0-based `nth`/`count`, a stray `anchor`, a `scroll` without
/// deltas, a malformed drag (mixed endpoint forms, missing end point), a
/// `count`/`held`/`duration_ms`/`x2`/`y2`/`to_selector` applied to an
/// action that does not take it, or a `duration_ms` under [`MIN_DRAG_MS`].
pub fn validate(step: &ComputerControl, tool: &str) -> Result<(), String> {
    let action = step.action.unwrap_or_default();
    // Partial coordinates are a mistake, never a "default the other axis" —
    // a lone `x` would otherwise dispatch at (x, 0).
    if step.x.is_some() != step.y.is_some() {
        return Err(format!("{tool}: coordinates require BOTH `x` and `y`"));
    }
    let has_point = step.x.is_some() || step.y.is_some();
    let has_element = step.selector.is_some()
        || step.app.is_some()
        || step.pid.is_some()
        || step.surface.is_some();

    // Exactly one target form (or neither, which only `up` allows).
    if has_point && has_element {
        return Err(format!(
            "{tool}: use coordinates (`x`/`y`) OR the element form \
             (`app`/`pid`/`surface` + `selector`), not both"
        ));
    }
    if action != PointerAction::Up && !has_point && !has_element {
        return Err(format!(
            "{tool}: action `{}` requires a target — coordinates (`x`/`y`) \
             or the element form (`app`/`pid`/`surface` + `selector`)",
            action_name(action)
        ));
    }
    // Element form needs a selector AND exactly one root (app scope or a
    // shell surface — phase-6 field test: a surface+selector step is the
    // same element form, not a scope leak).
    if step.selector.is_some()
        && step.app.is_none()
        && step.pid.is_none()
        && step.surface.is_none()
    {
        return Err(format!(
            "{tool}: the element form requires `app`, `pid` or `surface` together with `selector`"
        ));
    }
    if (step.app.is_some() || step.pid.is_some() || step.surface.is_some())
        && step.selector.is_none()
    {
        return Err(format!(
            "{tool}: `app`/`pid`/`surface` without `selector` would leak the scope — \
             pass `selector`, or use coordinates (`x`/`y`)"
        ));
    }
    if step.app.is_some() && (step.pid.is_some() || step.surface.is_some()) {
        return Err(format!(
            "{tool}: target ONE of `app`/`pid`/`surface` — they cannot share a step"
        ));
    }
    if step.pid.is_some() && step.surface.is_some() {
        return Err(format!(
            "{tool}: target ONE of `app`/`pid`/`surface` — they cannot share a step"
        ));
    }
    if step.nth == Some(0) {
        return Err(format!("{tool}: `nth` is 1-based; use 1 for the first match"));
    }
    if step.nth.is_some() && step.selector.is_none() {
        return Err(format!("{tool}: `nth` requires `selector`"));
    }
    if step.anchor.is_some() && step.selector.is_none() {
        return Err(format!(
            "{tool}: `anchor` applies to the element form and requires `selector`"
        ));
    }

    match action {
        PointerAction::Scroll if step.dx.is_none() && step.dy.is_none() => {
            return Err(format!("{tool}: action `scroll` requires `dx` and/or `dy`"));
        }
        PointerAction::Drag => validate_drag(step, tool)?,
        // Click actions accept `count` and `held`.
        PointerAction::Click | PointerAction::DoubleClick | PointerAction::RightClick => {}
        // `count` and `held` are click/drag concepts — a `move`/`scroll`/
        // `down`/`up` carrying them is a mistake the model should hear about.
        _ => {
            if step.count.is_some() {
                return Err(format!(
                    "{tool}: `count` applies to click actions, not `{}`",
                    action_name(action)
                ));
            }
            if step.held.is_some() {
                return Err(format!(
                    "{tool}: `held` applies to click and drag actions, not `{}`",
                    action_name(action)
                ));
            }
        }
    }
    if step.count == Some(0) {
        return Err(format!("{tool}: `count` is 1-based; use 1 for a single click"));
    }
    if let Some(duration) = step.duration_ms {
        if action != PointerAction::Drag {
            return Err(format!("{tool}: `duration_ms` applies only to `drag`"));
        }
        if duration < MIN_DRAG_MS {
            return Err(format!(
                "{tool}: `duration_ms` must be >= {MIN_DRAG_MS} (a shorter \
                 movement degenerates to a jump and drops hover events)"
            ));
        }
    }
    // Drag endpoints are drag-only — a click carrying `x2`/`y2`/`to_selector`
    // is a mistake the model should hear about, not a silently ignored field.
    if action != PointerAction::Drag
        && (step.x2.is_some() || step.y2.is_some() || step.to_selector.is_some())
    {
        return Err(format!(
            "{tool}: `x2`/`y2`/`to_selector` are drag endpoints and apply \
             only to `drag`, not `{}`",
            action_name(action)
        ));
    }
    Ok(())
}

/// `drag` endpoint validation: both ends in the SAME form, no mixing.
fn validate_drag(step: &ComputerControl, tool: &str) -> Result<(), String> {
    let coord_end = step.x2.is_some() || step.y2.is_some();
    let start_is_coord = step.x.is_some() || step.y.is_some();
    let start_is_element = step.selector.is_some();

    if coord_end && (step.x2.is_none() || step.y2.is_none()) {
        return Err(format!("{tool}: `drag` by coordinates requires both `x2` and `y2`"));
    }
    if coord_end && step.to_selector.is_some() {
        return Err(format!(
            "{tool}: `drag` end is either `x2`/`y2` OR `to_selector`, not both"
        ));
    }
    if start_is_element && coord_end {
        return Err(format!(
            "{tool}: `drag` endpoints must share a form — an element start pairs \
             with `to_selector`, a coordinate start with `x2`/`y2`"
        ));
    }
    if !start_is_element && step.to_selector.is_some() {
        return Err(format!(
            "{tool}: `to_selector` requires an element start (`app`/`pid`/`surface` + \
             `selector`)"
        ));
    }
    if start_is_element && step.to_selector.is_none() {
        return Err(format!(
            "{tool}: `drag` from an element requires `to_selector` for the end \
             point (element-to-element); use `x2`/`y2` with a coordinate start"
        ));
    }
    if start_is_coord && !coord_end {
        return Err(format!(
            "{tool}: `drag` requires an end point — `x2`/`y2` or `to_selector`"
        ));
    }
    Ok(())
}

/// Execute ONE validated pointer step. The element form resolves the point
/// from the element's CURRENT bounds at dispatch time (never stale); the
/// coordinate form passes through. Returns the step's report fragment.
pub fn run_step(
    sim: &xa11y::InputSim,
    step: &ComputerControl,
    tool: &str,
) -> Result<String, String> {
    let action = step.action.unwrap_or_default();
    let button = parse_button(step.button.as_deref(), tool)?;
    let held = keyboard::parse_keys(step.held.as_deref().unwrap_or_default(), tool)?;
    let count = step.count.unwrap_or(1);

    // Resolve the effective point for actions that need one. `up` and the
    // button-state half of `down` run at the current cursor position.
    let point: Option<Point> = if let Some(selector) = &step.selector {
        // resolve_element already renders through the error component with
        // the failing selector in context — no extra wrapper here.
        let element = resolve_element(step, selector, step.nth.unwrap_or(1), tool)?;
        Some(
            xa11y::point_for(&element, anchor(step.anchor.unwrap_or_default()))
                .map_err(|e| {
                    super::errors::render(tool, &format!("resolve point for `{selector}`"), &e)
                })?,
        )
    } else {
        match (step.x, step.y) {
            (Some(x), Some(y)) => Some(Point::new(x, y)),
            _ => None,
        }
    };

    // The drag's end point is resolved ONCE, before dispatch, and reused
    // for the report below — re-resolving `to_selector` after the drag ran
    // would let a UI change (e.g. the drop reordering the tree) turn a
    // successful drag into a spurious failure.
    let mut drag_to: Option<Point> = None;
    let result = match action {
        PointerAction::Click => sim.mouse().click_with(
            point_target(point),
            ClickOptions::new()
                .button(button)
                .count(count)
                .held(held),
        ),
        // `count` is honored on EVERY click action: double_click defaults to
        // 2, right_click defaults to 1 — passing an explicit `count` always
        // wins, so `right_click` + count 2 is a double right-click.
        PointerAction::DoubleClick => sim.mouse().click_with(
            point_target(point),
            ClickOptions::new()
                .button(button)
                .count(step.count.unwrap_or(2))
                .held(held),
        ),
        PointerAction::RightClick => sim.mouse().click_with(
            point_target(point),
            ClickOptions::new()
                .button(MouseButton::Right)
                .count(count)
                .held(held),
        ),
        PointerAction::Move => sim
            .mouse()
            .move_to(point.expect("validated: move requires a target")),
        // `down` presses at the CURRENT cursor position, so an explicit
        // target must be moved to FIRST — otherwise a drag would start at
        // wherever the cursor happens to be while the output still claims
        // the requested coordinates.
        PointerAction::Down => {
            if let Some(p) = point {
                sim.mouse()
                    .move_to(p)
                    .map_err(|e| super::errors::render(tool, "move pointer", &e))?;
            }
            sim.mouse().down(button)
        }
        PointerAction::Up => sim.mouse().up(button),
        PointerAction::Scroll => sim.mouse().scroll(
            point.expect("validated: scroll requires a target"),
            ScrollDelta {
                dx: step.dx.unwrap_or(0),
                dy: step.dy.unwrap_or(0),
            },
        ),
        PointerAction::Drag => {
            let from = point.expect("validated: drag requires a start target");
            let to = drag_end_point(step, tool)?;
            drag_to = Some(to);
            sim.mouse().drag_with(
                from,
                to,
                DragOptions::new()
                    .button(button)
                    .held(held)
                    .duration(Duration::from_millis(
                        step.duration_ms.unwrap_or(DEFAULT_DRAG_MS),
                    )),
            )
        }
    };
    result.map_err(|e| {
        super::errors::render(tool, &action_name(action).to_lowercase(), &e)
    })?;

    // Human-facing report with the EFFECTIVE point — for the element form
    // this is what the bounds resolved to at dispatch time, which makes a
    // mis-aimed click debuggable.
    Ok(match action {
        PointerAction::Click | PointerAction::DoubleClick | PointerAction::RightClick => {
            let verb = match action {
                PointerAction::Click => "clicked",
                PointerAction::DoubleClick => "double-clicked",
                _ => "right-clicked",
            };
            let extra = match action {
                PointerAction::Click if count > 1 => format!(" x{count}"),
                _ => String::new(),
            };
            format!("{verb} {}{extra}", where_at(step, point))
        }
        PointerAction::Move => format!("moved to {}", where_at(step, point)),
        PointerAction::Down => format!("button down {}", where_at(step, point)),
        PointerAction::Up => format!(
            "released {}",
            step.button
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("left")
        ),
        PointerAction::Scroll => format!(
            "scrolled ({},{}) {}",
            step.dx.unwrap_or(0),
            step.dy.unwrap_or(0),
            where_at(step, point)
        ),
        PointerAction::Drag => {
            // Reuse the end point resolved BEFORE dispatch (finding from
            // review): re-resolving `to_selector` here could fail after the
            // drag already ran and report a success as an error.
            let to = drag_to.expect("drag arm resolved its end point before dispatch");
            format!(
                "dragged {} → ({},{})",
                where_at(step, point),
                to.x,
                to.y
            )
        }
    })
}

/// Human-facing location of a pointer step's effect: the selector with its
/// resolved point, the bare point, or the cursor for targetless steps.
fn where_at(step: &ComputerControl, point: Option<Point>) -> String {
    match (step.selector.as_deref(), point) {
        (Some(sel), Some(p)) => format!("`{sel}` @{},{}", p.x, p.y),
        (Some(sel), None) => format!("`{sel}`"),
        (None, Some(p)) => format!("@{},{}", p.x, p.y),
        (None, None) => "at the current position".into(),
    }
}

/// Resolve ONE element under the step's root — a shell surface when
/// `surface` is set, the application otherwise (validated: exactly one of
/// app/pid/surface). Shared by the click/move/scroll point resolution and
/// the drag's `to_selector` end, so a future root addition lands in one
/// place.
fn resolve_element(
    step: &ComputerControl,
    selector: &str,
    nth: usize,
    tool: &str,
) -> Result<xa11y::Element, String> {
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    if let Some(surface_kind) = step.surface {
        super::surface::resolve(surface_kind, timeout)
            .map_err(|e| format!("{tool}: {e}"))?
            .locator(selector)
            .nth(nth)
            .element()
            .map_err(|e| super::errors::render(tool, "resolve element", &e))
    } else {
        resolve_app(step, tool)?
            .locator(selector)
            .nth(nth)
            .element()
            .map_err(|e| super::errors::render(tool, "resolve element", &e))
    }
}

/// The drag's end point in the SAME form as the start (validated):
/// element → element resolves `to_selector` on the same root at dispatch
/// time; coordinates → coordinates pass through.
fn drag_end_point(step: &ComputerControl, tool: &str) -> Result<Point, String> {
    if let Some(to_selector) = &step.to_selector {
        // resolve_element already renders through the error component —
        // no extra wrapper here.
        let element = resolve_element(step, to_selector, 1, tool)?;
        xa11y::point_for(&element, anchor(step.anchor.unwrap_or_default()))
            .map_err(|e| super::errors::render(tool, "resolve drag end point", &e))
    } else {
        Ok(Point::new(
            step.x2.expect("validated: drag end coordinates"),
            step.y2.expect("validated: drag end coordinates"),
        ))
    }
}

/// Wrap the resolved point into a [`ClickTarget`] (both forms land as a
/// point by the time the click runs — the element's bounds were just read).
fn point_target(point: Option<Point>) -> ClickTarget<'static> {
    ClickTarget::Point(point.unwrap_or(Point::new(0, 0)))
}

/// Resolve the target application (validating `app`/`pid` exclusivity).
fn resolve_app(step: &ComputerControl, tool: &str) -> Result<App, String> {
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    let name = step.app.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (name, step.pid) {
        (Some(name), None) => App::by_name(name, timeout)
            .map_err(|e| super::errors::render(tool, "resolve application", &e)),
        (None, Some(pid)) => App::by_pid(pid, timeout)
            .map_err(|e| super::errors::render(tool, "resolve application", &e)),
        (None, None) => Err(format!("{tool}: the element form requires `app` or `pid`")),
        (Some(_), Some(_)) => Err(format!("{tool}: provide `app` or `pid`, not both")),
    }
}

/// Parse a button name (`left`/`right`/`middle`).
pub fn parse_button(name: Option<&str>, tool: &str) -> Result<MouseButton, String> {
    match name.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("left") | Some("") => Ok(MouseButton::Left),
        Some("right") => Ok(MouseButton::Right),
        Some("middle") => Ok(MouseButton::Middle),
        Some(other) => Err(format!(
            "{tool}: unknown button `{other}` (expected left, right or middle)"
        )),
    }
}

/// Map the wire anchor to xa11y's [`Anchor`].
pub fn anchor(value: PointerAnchor) -> Anchor {
    match value {
        PointerAnchor::Center => Anchor::Center,
        PointerAnchor::TopLeft => Anchor::TopLeft,
        PointerAnchor::TopRight => Anchor::TopRight,
        PointerAnchor::BottomLeft => Anchor::BottomLeft,
        PointerAnchor::BottomRight => Anchor::BottomRight,
    }
}

/// Wire-facing name of a pointer action.
pub fn action_name(action: PointerAction) -> &'static str {
    match action {
        PointerAction::Click => "click",
        PointerAction::DoubleClick => "double_click",
        PointerAction::RightClick => "right_click",
        PointerAction::Move => "move",
        PointerAction::Down => "down",
        PointerAction::Up => "up",
        PointerAction::Scroll => "scroll",
        PointerAction::Drag => "drag",
    }
}

#[cfg(test)]
mod pointer_validation_tests {
    use super::{validate, MIN_DRAG_MS};
    use crate::computer::types::{ComputerControl, PointerAction, PointerAnchor};

    const TOOL: &str = "computer_control";

    fn step() -> ComputerControl {
        ComputerControl::default()
    }

    #[test]
    fn element_and_coordinate_steps_pass() {
        let element = ComputerControl {
            app: Some("Safari".into()),
            selector: Some("button[name='OK']".into()),
            ..step()
        };
        let coordinate = ComputerControl {
            x: Some(10),
            y: Some(20),
            ..step()
        };
        assert!(validate(&element, TOOL).is_ok());
        assert!(validate(&coordinate, TOOL).is_ok());
    }

    #[test]
    fn scroll_requires_delta() {
        let s = ComputerControl {
            action: Some(PointerAction::Scroll),
            x: Some(1),
            y: Some(1),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`dx` and/or `dy`"), "err: {err}");
    }

    #[test]
    fn drag_mixed_endpoints_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Drag),
            x: Some(1),
            y: Some(1),
            x2: Some(2),
            y2: Some(2),
            to_selector: Some("other".into()),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn nth_zero_rejected() {
        let s = ComputerControl {
            app: Some("Safari".into()),
            selector: Some("button".into()),
            nth: Some(0),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");
    }

    #[test]
    fn count_zero_rejected() {
        let s = ComputerControl {
            x: Some(1),
            y: Some(1),
            count: Some(0),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");
    }

    #[test]
    fn click_without_target_rejected() {
        let err = validate(&step(), TOOL).unwrap_err();
        assert!(err.contains("requires a target"), "err: {err}");
    }

    #[test]
    fn anchor_without_selector_rejected() {
        let s = ComputerControl {
            x: Some(1),
            y: Some(1),
            anchor: Some(PointerAnchor::TopLeft),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`anchor`"), "err: {err}");
    }

    #[test]
    fn move_with_count_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Move),
            x: Some(1),
            y: Some(1),
            count: Some(2),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`count` applies to click"), "err: {err}");
    }

    #[test]
    fn short_drag_duration_rejected() {
        let s = ComputerControl {
            action: Some(PointerAction::Drag),
            x: Some(1),
            y: Some(1),
            x2: Some(2),
            y2: Some(2),
            duration_ms: Some(MIN_DRAG_MS - 1),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("duration_ms"), "err: {err}");
    }
}
