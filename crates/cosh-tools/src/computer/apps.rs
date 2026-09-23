//! `computer_apps` — list running desktop applications, or report the
//! foreground application and its keyboard-focused element.
//!
//! Thin adapter over [`xa11y::AppExt`]; every xa11y call is blocking
//! (platform accessibility APIs are synchronous) so it runs on tokio's
//! blocking pool.
use std::time::Duration;

use serde::Serialize;
use xa11y::{App, AppExt};

use super::types::{
    AppInfo, AppsOutput, AppsTarget, FocusedElement, FocusedOutput, ComputerApps,
};

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
    let app = App::foreground(timeout)
        .map_err(|e| super::errors::render("computer_apps", "resolve foreground application", &e))?;
    let focused_element = find_focused_below(&app.as_element()).map(|(path, leaf)| FocusedElement {
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
const FOCUSED_WALK_MAX_DEPTH: usize = 15;

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
fn find_focused_below(root: &xa11y::Element) -> Option<(Vec<String>, xa11y::ElementData)> {
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
fn find_focused(
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

#[cfg(test)]
mod apps_focus_tests {
    use std::sync::Arc;

    use xa11y::mock::build_provider;
    use xa11y::{Element, Provider, Role};

    use super::super::types::{AppsTarget, ComputerApps};
    use super::{find_focused, find_focused_below, FOCUSED_WALK_MAX_DEPTH};

    /// The mock fixture's window reports `focused` — the walk below the app
    /// root must find it and report the role path from the root down to the
    /// leaf, inclusive (both snake_case). The root's own (synthetic)
    /// foreground stamp must not match: the answer is the window, not the
    /// application node.
    #[test]
    fn walk_finds_focused_element_with_role_path() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, Arc::clone(&provider));

        let (path, leaf) = find_focused_below(&app_root)
            .expect("the fixture window holds focus, so the walk must find it");
        assert_eq!(path, vec!["application", "window"]);
        assert_eq!(leaf.role, Role::Window);
        assert!(leaf.states.focused);
    }

    /// xa11y's `foreground_with` stamps `states.focused = true` on the app
    /// root it returns (a *foreground-app* marker, not keyboard focus). The
    /// walk below must ignore that stamp: with the root stamped — exactly
    /// what production sees — the answer is still the focused WINDOW, never
    /// the root itself.
    #[test]
    fn walk_ignores_the_roots_synthetic_focus_stamp() {
        let provider: Arc<dyn Provider> = build_provider();
        let mut root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        root_data.states.focused = true; // what foreground_with stamps
        let app_root = Element::new(root_data, provider);

        let (path, leaf) =
            find_focused_below(&app_root).expect("the focused window sits below the root");
        assert_eq!(path, vec!["application", "window"]);
        assert_eq!(leaf.role, Role::Window);
    }

    /// A subtree with no focused element yields `None` — rendered later as
    /// `focused_element: null`, never an error.
    #[test]
    fn walk_returns_none_when_nothing_is_focused() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, provider);
        let window = app_root.children().expect("window child")[0].clone();
        let toolbar = window.children().expect("toolbar child")[0].clone();

        assert!(find_focused(&toolbar, 0).is_none());
    }

    /// The walk honors the depth bound: past the cap it reports absence
    /// instead of walking unbounded trees — here the cap bites before the
    /// (focused) window is even examined.
    #[test]
    fn walk_stops_at_depth_cap() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, provider);
        let window = app_root.children().expect("window child")[0].clone();

        assert!(find_focused(&window, FOCUSED_WALK_MAX_DEPTH + 1).is_none());
        // Sanity: the same element at depth 0 IS found — the cap, not the
        // element, is what made the walk stop.
        assert!(find_focused(&window, 0).is_some());
    }

    /// Default target is `all` — an empty input keeps the old listing
    /// behavior, so existing callers passing `{}` are unaffected.
    #[test]
    fn default_target_is_all() {
        let input: ComputerApps = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(input.target.unwrap_or_default(), AppsTarget::All);
        let explicit: ComputerApps =
            serde_json::from_value(serde_json::json!({ "target": "focused" })).unwrap();
        assert_eq!(explicit.target.unwrap_or_default(), AppsTarget::Focused);
    }
}
