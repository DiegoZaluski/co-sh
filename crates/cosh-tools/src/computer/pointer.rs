//! `computer_pointer` — pointer actions at raw desktop coordinates.
//!
//! The coordinate-based twin of [`super::touch`]: use it when the target
//! has no reliable accessibility node (canvas, custom widget) and the
//! coordinates come from a `computer_screenshot` image mapped to the desktop via
//! its `desktop_origin`/`desktop_scale`. Every call is blocking (synthetic
//! input event), so it runs on tokio's blocking pool.
use xa11y::{input_sim, MouseButton, Point, ScrollDelta};

use super::types::{PointerAction, PointerOutput, ComputerPointer};

/// Click / move / scroll at desktop coordinates.
///
/// `x`/`y` are required for every action except `up` (which releases the
/// button at the current cursor position). `scroll` additionally requires
/// at least one of `dx`/`dy`. `down`/`up` accept a `button`
/// (`left`/`right`/`middle`, default left); clicks are always left-button.
///
/// # Errors
///
/// Returns `Err` for missing coordinates or scroll delta, an unknown
/// `button` name, or when the platform input backend is unavailable (e.g.
/// missing accessibility/input permission).
pub async fn pointer(input: &ComputerPointer) -> Result<PointerOutput, String> {
    let action = input.action.unwrap_or_default();
    if action != PointerAction::Up && (input.x.is_none() || input.y.is_none()) {
        return Err(format!(
            "computer_pointer: action `{}` requires both `x` and `y`",
            action_name(action)
        ));
    }
    if action == PointerAction::Scroll && input.dx.is_none() && input.dy.is_none() {
        return Err("computer_pointer: action `scroll` requires `dx` and/or `dy`".into());
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || pointer_blocking(&input))
        .await
        .map_err(|e| format!("computer_pointer: blocking task failed: {e}"))?
}

fn pointer_blocking(input: &ComputerPointer) -> Result<PointerOutput, String> {
    let action = input.action.unwrap_or_default();
    let sim = shared_input_sim().map_err(|e| format!("computer_pointer: input backend: {e}"))?;
    let point = match (input.x, input.y) {
        (Some(x), Some(y)) => Some(Point::new(x, y)),
        _ => None,
    };

    let result = match action {
        PointerAction::Click => {
            sim.mouse().click(point.unwrap_or(Point::new(0, 0)))
        }
        PointerAction::DoubleClick => {
            sim.mouse().double_click(point.unwrap_or(Point::new(0, 0)))
        }
        PointerAction::RightClick => {
            sim.mouse().right_click(point.unwrap_or(Point::new(0, 0)))
        }
        PointerAction::Move => sim.mouse().move_to(point.unwrap_or(Point::new(0, 0))),
        // `down` presses at the CURRENT cursor position, so an explicit
        // target must be moved to FIRST — otherwise a drag would start at
        // wherever the cursor happens to be while the output still claims
        // the requested coordinates.
        PointerAction::Down => {
            let button = parse_button(input.button.as_deref())?;
            if let Some(p) = point {
                sim.mouse()
                    .move_to(p)
                    .map_err(|e| format!("computer_pointer: move_to: {e}"))?;
            }
            sim.mouse().down(button)
        }
        PointerAction::Up => sim.mouse().up(parse_button(input.button.as_deref())?),
        PointerAction::Scroll => sim.mouse().scroll(
            point.unwrap_or(Point::new(0, 0)),
            ScrollDelta {
                dx: input.dx.unwrap_or(0),
                dy: input.dy.unwrap_or(0),
            },
        ),
    };
    result.map_err(|e| format!("computer_pointer: {}: {e}", action_name(action)))?;

    Ok(PointerOutput {
        action,
        x: input.x,
        y: input.y,
    })
}

/// Process-wide input simulator.
///
/// The Wayland backend owns a `uinput` device created per `InputSim`;
/// rebuilding it per call would split a `down`/`up` (or drag) sequence
/// across devices and lose the held button. Sharing one instance keeps
/// button state continuous across tool calls.
fn shared_input_sim() -> Result<xa11y::InputSim, String> {
    static SIM: std::sync::OnceLock<Result<xa11y::InputSim, String>> = std::sync::OnceLock::new();
    SIM.get_or_init(|| input_sim().map_err(|e| format!("computer_pointer: input backend: {e}")))
        .clone()
}

/// Parse a button name (`left`/`right`/`middle`).
fn parse_button(name: Option<&str>) -> Result<MouseButton, String> {
    match name.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("left") | Some("") => Ok(MouseButton::Left),
        Some("right") => Ok(MouseButton::Right),
        Some("middle") => Ok(MouseButton::Middle),
        Some(other) => Err(format!(
            "computer_pointer: unknown button `{other}` (expected left, right or middle)"
        )),
    }
}

/// Wire-facing name of a pointer action.
fn action_name(action: PointerAction) -> &'static str {
    match action {
        PointerAction::Click => "click",
        PointerAction::DoubleClick => "double_click",
        PointerAction::RightClick => "right_click",
        PointerAction::Move => "move",
        PointerAction::Down => "down",
        PointerAction::Up => "up",
        PointerAction::Scroll => "scroll",
    }
}

