//! `computer_apps` — list running desktop applications, or report the
//! foreground application and its keyboard-focused element.
//!
//! Thin adapter over [`xa11y::AppExt`]; every xa11y call is blocking
//! (platform accessibility APIs are synchronous) so it runs on tokio's
//! blocking pool.
use std::time::Duration;

use serde::Serialize;
use xa11y::{App, AppExt};

use super::types::{AppInfo, AppsOutput, AppsTarget, ComputerApps, FocusedElement, FocusedOutput};

/// Default auto-wait for a foreground application to exist.
const DEFAULT_TIMEOUT_MS: u64 = 3000;

/// Result of `computer_apps`, by [`AppsTarget`].
///
/// Untagged: the model sees the plain `AppsOutput` or `FocusedOutput` JSON
/// shape, never the enum wrapper.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AppsResult {
    /// `target: "all"` (default) — every running application.
    All(AppsOutput),
    /// `target: "focused"` — the foreground app and its focused element.
    Focused(FocusedOutput),
}

/// Report running desktop applications, or the focused one.
///
/// With `target: "all"` (the default) the result is a point-in-time snapshot
/// in platform enumeration order; the entry actually holding the system
/// foreground is flagged.
///
/// With `target: "focused"` only the foreground application is resolved, plus
/// the element inside it holding keyboard focus — the cheap "where do my
/// keystrokes go?" check before `computer_keyboard`.
///
/// # Errors
///
/// Returns `Err` when the platform accessibility API is unreachable
/// (missing macOS Accessibility permission, no AT-SPI2 bus on Linux, …), or —
/// for `target: "focused"` — when no application holds the foreground within
/// the timeout (e.g. focus is on the shell or a lock screen).
pub async fn apps(input: &ComputerApps) -> Result<AppsResult, String> {
    let input = input.clone();
    tokio::task::spawn_blocking(move || apps_blocking(&input))
        .await
        .map_err(|e| format!("computer_apps: blocking task failed: {e}"))?
}

fn apps_blocking(input: &ComputerApps) -> Result<AppsResult, String> {
    match input.target.unwrap_or_default() {
        AppsTarget::All => list_all().map(AppsResult::All),
        AppsTarget::Focused => focused_blocking(input).map(AppsResult::Focused),
    }
}

fn list_all() -> Result<AppsOutput, String> {
    let list =
        App::list().map_err(|e| super::errors::render("computer_apps", "list applications", &e))?;
    let apps: Vec<AppInfo> = list
        .iter()
        .map(|app| AppInfo {
            name: app.name.clone(),
            pid: app.pid,
            foreground: app.is_foreground(),
        })
        .collect();
    let count = apps.len();
    Ok(AppsOutput { apps, count })
}

/// Resolve the foreground application and the element inside it holding
/// keyboard focus.
///
/// The focused element is found by a bounded depth-first walk looking for
/// `states.focused` — the same state the snapshot reports, so what this
/// answers is exactly what `computer_keyboard` will type into. A miss (focus
/// on the window itself, or the platform not reporting element focus)
/// reports `focused_element: null` rather than failing, because the app
/// answer is still correct and useful.
fn focused_blocking(input: &ComputerApps) -> Result<FocusedOutput, String> {
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let app = App::foreground(timeout).map_err(|e| {
        super::errors::render("computer_apps", "resolve foreground application", &e)
    })?;
    let focused_element =
        find_focused_below(&app.as_element()).map(|(path, leaf)| FocusedElement {
            role: leaf.role.to_snake_case().to_string(),
            name: leaf.name.clone(),
            value: leaf.value.clone(),
            path,
        });
    Ok(FocusedOutput {
        app: app.name.clone(),
        pid: app.pid,
        focused_element,
    })
}

/// Depth bound for the focused-element walk: deep enough to reach fields in
/// real dialogs, shallow enough to stay cheap. A tree deeper than this does
/// not fail the query — focus that far down is reported as absent, and a
/// snapshot answers it precisely.
pub(crate) const FOCUSED_WALK_MAX_DEPTH: usize = 15;

/// Depth-first search for the focused element **below** `root`, with
/// `root`'s own `states.focused` flag ignored.
///
/// Skipping the root's flag is essential: xa11y's `foreground_with` stamps
/// `states.focused = true` on the application root it returns (so
/// `App::is_foreground()` agrees with `list`/`find`), and that stamp is the
/// *foreground-app* marker, not element-level keyboard focus. Matching it
/// would short-circuit the walk and always report the app root as the
/// focused element.
///
/// Returns the leaf's data plus the role path from the root down to it,
/// inclusive.
pub(crate) fn find_focused_below(
    root: &xa11y::Element,
) -> Option<(Vec<String>, xa11y::ElementData)> {
    let root_role = root.data().role.to_snake_case().to_string();
    for child in root.children().ok()? {
        if let Some((path, leaf)) = find_focused(&child, 1) {
            let mut full = Vec::with_capacity(path.len() + 1);
            full.push(root_role);
            full.extend(path);
            return Some((full, leaf));
        }
    }
    None
}

/// Depth-first search for the element whose state reports keyboard focus,
/// starting at `depth`. Returns the leaf's data plus the role path from (and
/// including) this element down to it.
pub(crate) fn find_focused(
    element: &xa11y::Element,
    depth: usize,
) -> Option<(Vec<String>, xa11y::ElementData)> {
    if depth > FOCUSED_WALK_MAX_DEPTH {
        return None;
    }
    let data = element.data();
    let role = data.role.to_snake_case().to_string();
    if data.states.focused {
        return Some((vec![role], data.clone()));
    }
    // Cap reached: report absence instead of fetching another level —
    // bounded depth means bounded provider work.
    if depth == FOCUSED_WALK_MAX_DEPTH {
        return None;
    }
    for child in element.children().ok()? {
        if let Some((mut path, leaf)) = find_focused(&child, depth + 1) {
            path.insert(0, role.clone());
            return Some((path, leaf));
        }
    }
    None
}
