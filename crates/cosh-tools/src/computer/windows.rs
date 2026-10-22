//! `computer_windows` — list the windows of ONE application, or operate on
//! one of them (activate/minimize/maximize/restore/close/move/resize).
//!
//! The window listing is the bridge from app-level ambiguity ("which of
//! this app's four windows...?") to element targets: every entry carries a
//! role, title, bounds and window state, so an `action` can be aimed at a
//! specific window with `selector`.
//!
//! Thin adapter over [`xa11y::App::windows`] and xa11y's window verbs; every
//! xa11y call is blocking (platform accessibility APIs are synchronous), so
//! it runs on tokio's blocking pool like every other tool in this module.
use std::time::Duration;

use xa11y::{App, AppExt};

use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{
    ComputerWindows, WindowAction, WindowBounds, WindowInfo, WindowManageOutput, WindowsOutput,
    WindowsResult,
};

/// Wire-facing name of a window action (matches the snake_case serde
/// renaming); reused in the manage output.
pub fn action_name(action: WindowAction) -> &'static str {
    match action {
        WindowAction::Activate => "activate",
        WindowAction::Minimize => "minimize",
        WindowAction::Maximize => "maximize",
        WindowAction::EnterFullscreen => "enter_fullscreen",
        WindowAction::Restore => "restore",
        WindowAction::Close => "close",
        WindowAction::Move => "move",
        WindowAction::Resize => "resize",
    }
}

/// Input validation for `computer_windows`: exactly one app scope, optional
/// selector with a 1-based `nth`, and payload pairing for `move`/`resize`.
///
/// # Errors
///
/// Returns `Err` for `name` and `pid` together, no app scope at all, a
/// 0-based `nth`, `nth` without `selector`, `move` without both `x` and
/// `y` (or with `width`/`height`), `resize` without both `width` and
/// `height` (or with `x`/`y`), and `x`/`y` or `width`/`height` given
/// without the matching action.
pub fn validate(input: &ComputerWindows) -> Result<(), String> {
    const TOOL: &str = "computer_windows";
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
    if input.nth.is_some() && input.selector.is_none() {
        return Err(format!("{TOOL}: `nth` requires `selector`"));
    }
    // BOTH pairing rules first — a lone coordinate would otherwise slip
    // through to dispatch and coerce the missing member to 0 (a window
    // silently moved to (x, 0)), exactly like the pointer engine's
    // `x`-without-`y` guard.
    if input.x.is_some() != input.y.is_some() {
        return Err(format!(
            "{TOOL}: action `move` takes BOTH `x` and `y` (desktop pixels)"
        ));
    }
    if input.width.is_some() != input.height.is_some() {
        return Err(format!(
            "{TOOL}: action `resize` takes BOTH `width` and `height` (desktop pixels)"
        ));
    }
    let payload_move = input.x.is_some();
    let payload_size = input.width.is_some();
    match input.action {
        Some(WindowAction::Move) => {
            if !payload_move {
                return Err(format!("{TOOL}: action `move` requires both `x` and `y`"));
            }
            if payload_size {
                return Err(format!(
                    "{TOOL}: `move` takes `x`/`y`; `width`/`height` belong to `resize`"
                ));
            }
        }
        Some(WindowAction::Resize) => {
            if !payload_size {
                return Err(format!(
                    "{TOOL}: action `resize` requires both `width` and `height`"
                ));
            }
            if payload_move {
                return Err(format!(
                    "{TOOL}: `resize` takes `width`/`height`; `x`/`y` belong to `move`"
                ));
            }
        }
        _ => {
            if payload_move {
                return Err(format!(
                    "{TOOL}: `x`/`y` belong to action `move` — pair them with `action` (`width`/`height` with `resize`)"
                ));
            }
            if payload_size {
                return Err(format!(
                    "{TOOL}: `width`/`height` belong to action `resize` — pair them with `action` (`x`/`y` with `move`)"
                ));
            }
        }
    }
    Ok(())
}

/// Run `computer_windows` on tokio's blocking pool.
///
/// # Errors
///
/// Returns `Err` for invalid input, an app that never surfaces, a
/// `selector` that never matches, a window action the platform rejects, or
/// when a manage target needs a window but the app currently has none
/// (e.g. it closed in the meantime).
pub async fn windows(input: &ComputerWindows) -> Result<WindowsResult, String> {
    let input = input.clone();
    tokio::task::spawn_blocking(move || windows_blocking(&input))
        .await
        .map_err(|e| format!("computer_windows: blocking task failed: {e}"))?
}

fn windows_blocking(input: &ComputerWindows) -> Result<WindowsResult, String> {
    const TOOL: &str = "computer_windows";
    validate(input)?;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout),
        (None, Some(pid)) => App::by_pid(pid, timeout),
        _ => unreachable!("validated: computer_windows carries exactly one app scope"),
    }
    .map_err(|e| super::errors::render_app_miss(TOOL, &e))?;

    match input.action {
        None => list(&app, input),
        Some(action) => manage(&app, input, action),
    }
}

/// List mode: every window of the application (enumeration order), or the
/// single element matched by `selector`. `pub(crate)` so the mock-provider
/// tests exercise it against a directly-built `App`.
pub(crate) fn list(app: &App, input: &ComputerWindows) -> Result<WindowsResult, String> {
    const TOOL: &str = "computer_windows";
    let selector = input
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let windows: Vec<WindowInfo> = match selector {
        None => app
            .windows()
            .map_err(|e| super::errors::render(TOOL, "list windows", &e))?
            .iter()
            .map(|element| window_info(element.data()))
            .collect(),
        Some(selector) => {
            let element = app
                .locator(selector)
                .nth(input.nth.unwrap_or(1))
                .element()
                .map_err(|e| super::errors::render(TOOL, "resolve selector", &e))?;
            vec![window_info(element.data())]
        }
    };
    let count = windows.len();
    Ok(WindowsResult::List(WindowsOutput {
        app: app.name.clone(),
        pid: app.pid,
        windows,
        count,
    }))
}

/// Manage mode: resolve ONE window (selector or the app's first), perform
/// the action, then re-observe the same window so the output reports the
/// state the action produced. `pub(crate)` for the mock-provider tests.
pub(crate) fn manage(
    app: &App,
    input: &ComputerWindows,
    action: WindowAction,
) -> Result<WindowsResult, String> {
    const TOOL: &str = "computer_windows";
    let selector = input
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // (target element, its index in windows() order — known only for the
    // default first-window targeting; the re-observation below matches by
    // title when known and by index otherwise.)
    let (element, listed_index) = match selector {
        Some(selector) => {
            let element = app
                .locator(selector)
                .nth(input.nth.unwrap_or(1))
                .element()
                .map_err(|e| super::errors::render(TOOL, "resolve selector", &e))?;
            (element, None)
        }
        None => {
            let windows = app
                .windows()
                .map_err(|e| super::errors::render(TOOL, "list windows", &e))?;
            let element = windows.into_iter().next().ok_or_else(|| {
                format!("{TOOL}: the application has no window to act on right now")
            })?;
            (element, Some(0))
        }
    };

    let result = match action {
        WindowAction::Activate => element.activate(),
        WindowAction::Minimize => element.minimize(),
        WindowAction::Maximize => element.maximize(),
        WindowAction::EnterFullscreen => element.enter_fullscreen(),
        WindowAction::Restore => element.restore(),
        WindowAction::Close => {
            element
                .close()
                .map_err(|e| super::errors::render(TOOL, action_name(action), &e))?;
            // The window is (about to be) gone by the time the call
            // returns; re-observing it would be the stale-data trap, so a
            // `close` reports no post-action window state.
            return Ok(WindowsResult::Manage(WindowManageOutput {
                action: action_name(action).to_string(),
                window: None,
            }));
        }
        WindowAction::Move => element.move_to(
            input.x.expect("validated: move carries both coordinates"),
            input.y.expect("validated: move carries both coordinates"),
        ),
        WindowAction::Resize => element.resize_to(
            input.width.expect("validated: resize carries both dimensions"),
            input.height.expect("validated: resize carries both dimensions"),
        ),
    };
    result.map_err(|e| super::errors::render(TOOL, action_name(action), &e))?;

    // Post-observation: re-list (the platform's live state, never the
    // stale snapshot the target Element carries) and re-find the SAME
    // window — by title when the target had one, else by its listing
    // position.
    let listed = listed_index;
    let title = element.data().name.clone();
    let window = app.windows().ok().and_then(|windows| {
        let matched = title
            .as_ref()
            .and_then(|name| {
                windows
                    .iter()
                    .position(|w| w.data().name.as_ref() == Some(name))
            })
            .or(listed
                .filter(|_| title.is_none())
                .filter(|i| *i < windows.len()));
        matched.map(|i| window_info(windows[i].data()))
    });
    Ok(WindowsResult::Manage(WindowManageOutput {
        action: action_name(action).to_string(),
        window,
    }))
}

/// Normalize one element's data into the wire shape. Window geometry
/// mapping: listing bounds and `move`/`resize` share the same
/// desktop-pixel space (physical px on Windows, logical points elsewhere),
/// so a listed `bounds` can be fed straight back into an action.
pub(crate) fn window_info(data: &xa11y::ElementData) -> WindowInfo {
    WindowInfo {
        role: data.role.to_snake_case().to_string(),
        name: data.name.clone(),
        bounds: data.bounds.map(|b| WindowBounds {
            x: b.x,
            y: b.y,
            width: b.width,
            height: b.height,
        }),
        active: Some(data.states.active),
        minimized: data.states.minimized,
        maximized: data.states.maximized,
        fullscreen: data.states.fullscreen,
    }
}
