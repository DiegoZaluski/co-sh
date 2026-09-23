//! `computer_snapshot` — capture an application's accessibility tree.
//!
//! Two renderings share one resolution path: the compact indented outline
//! (role, name, value and non-default state flags per line) and structured
//! JSON ([`StateNode`] serialized directly, states included). The tool
//! builds its own tree from `Element::children()` + `ElementData.states`
//! because xa11y's `TreeNode` carries no states.
//!
//! Resolution is fail-fast: only the application lookup auto-waits (up to
//! `timeout_ms`). Selector matching resolves once — a selector on an
//! element that is not yet in the tree fails immediately; re-call after
//! the interface settles instead of expecting the tool to poll.
use std::time::Duration;

use xa11y::{App, AppExt};

use super::types::{ElementStates, SnapshotFormat, SnapshotOutput, StateNode, ComputerSnapshot};

/// Default auto-wait for the application to appear.
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 3000;
/// Default snapshot depth — deep enough for real dialogs, shallow enough
/// to keep the output inside a tool-result budget.
const DEFAULT_MAX_DEPTH: u32 = 12;
/// Hard depth cap: runaway web-embedded trees can be enormous.
const MAX_MAX_DEPTH: u32 = 40;
/// Hard node budget for a snapshot. Depth alone does not bound size: a
/// shallow-but-wide tree (huge list, text editor) can still hold millions
/// of elements.
const MAX_NODES: usize = 4000;

/// Capture the accessibility tree of one application.
///
/// Resolves the app by `name` or `pid` (exactly one required), optionally
/// narrows to the subtree matching `selector`, and renders the result as
/// either an indented outline or structured JSON.
///
/// # Errors
///
/// Returns `Err` for invalid input (name and pid both set or both missing,
/// `nth` set without `selector`, `nth` of 0), when the application is not
/// found within the timeout, when the selector does not match, when the
/// snapshot exceeds [`MAX_NODES`], or when the platform accessibility API
/// is unreachable.
pub async fn snapshot(input: &ComputerSnapshot) -> Result<SnapshotOutput, String> {
    super::surface::validate_snapshot(input)?;
    if input.nth == Some(0) {
        return Err("computer_snapshot: `nth` is 1-based; use 1 for the first match".into());
    }
    if input.nth.is_some() && input.selector.is_none() {
        return Err("computer_snapshot: `nth` requires `selector`".into());
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || snapshot_blocking(&input))
        .await
        .map_err(|e| format!("computer_snapshot: blocking task failed: {e}"))?
}

fn snapshot_blocking(input: &ComputerSnapshot) -> Result<SnapshotOutput, String> {
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let max_depth = Some(
        input
            .max_depth
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .min(MAX_MAX_DEPTH) as usize,
    );
    let format = input.format.unwrap_or_default();

    // Shell-surface targeting: the SAME selector flow, rooted at the
    // surface instead of an app (validated: exactly one of name/pid/surface).
    if let Some(surface_kind) = input.surface {
        let surface = super::surface::resolve(surface_kind, timeout)
            .map_err(|e| format!("computer_snapshot: {e}"))?;
        return snapshot_surface(input, surface, max_depth, format);
    }

    let name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout),
        (None, Some(pid)) => App::by_pid(pid, timeout),
        _ => unreachable!("validated in snapshot"),
    }
    .map_err(|e| super::errors::render("computer_snapshot", "resolve application", &e))?;

    let (snapshot, elements) = match &input.selector {
        Some(selector) => {
            let nth = input.nth.unwrap_or(1);
            let locator = app.locator(selector).nth(nth);
            render(&format, &locator, max_depth)?
        }
        None => render_root(&format, &app.as_element(), max_depth)?,
    };

    Ok(SnapshotOutput {
        app: app.name.clone(),
        pid: app.pid,
        snapshot,
        elements,
    })
}

/// Render a surface-rooted snapshot — shared by the production path
/// (resolved against the singleton provider) and the tests (mock provider,
/// via the `ShellSurface` handle they build directly).
fn snapshot_surface(
    input: &ComputerSnapshot,
    surface: xa11y::ShellSurface,
    max_depth: Option<usize>,
    format: SnapshotFormat,
) -> Result<SnapshotOutput, String> {
    let (snapshot, elements) = match &input.selector {
        Some(selector) => {
            let nth = input.nth.unwrap_or(1);
            let locator = surface.locator(selector).nth(nth);
            render(&format, &locator, max_depth)?
        }
        None => render_root(&format, &surface.as_element(), max_depth)?,
    };
    Ok(SnapshotOutput {
        app: surface.name.clone(),
        pid: surface.pid,
        snapshot,
        elements,
    })
}

/// Render a locator-rooted snapshot in the requested format.
fn render(
    format: &SnapshotFormat,
    locator: &xa11y::Locator,
    max_depth: Option<usize>,
) -> Result<(String, usize), String> {
    let root = locator
        .element()
        .map_err(|e| super::errors::render("computer_snapshot", "resolve selector", &e))?;
    let tree = build_state_tree(&root, max_depth, 0)?;
    let count = checked_node_count(&tree)?;
    Ok((render_tree(format, &tree), count))
}

/// Render a snapshot rooted at ANY element handle — an application root or
/// a shell-surface root take the same path (both wrap a real platform
/// element).
fn render_root(
    format: &SnapshotFormat,
    root: &xa11y::Element,
    max_depth: Option<usize>,
) -> Result<(String, usize), String> {
    let tree = build_state_tree(root, max_depth, 0)?;
    let count = checked_node_count(&tree)?;
    Ok((render_tree(format, &tree), count))
}

/// Build the tool's own tree with states, mirroring xa11y's
/// `build_tree_node` depth semantics: `Some(d)` stops at depth `d`,
/// `None` traverses the full subtree.
///
/// States come from the same `ElementData` fetch that identifies the node —
/// one provider round trip per element, same as xa11y's stateless tree.
fn build_state_tree(
    element: &xa11y::Element,
    max_depth: Option<usize>,
    depth: usize,
) -> Result<StateNode, String> {
    let data = element.data();
    let states = ElementStates::from_state_set(&data.states);
    let children = if max_depth.is_none_or(|d| depth < d) {
        element
            .children()
            .map_err(|e| super::errors::render("computer_snapshot", "read tree", &e))?
            .into_iter()
            .map(|child| build_state_tree(&child, max_depth, depth + 1))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(StateNode {
        role: data.role.to_snake_case().to_string(),
        name: data.name.clone(),
        value: data.value.clone(),
        states,
        children,
    })
}

/// Serialize a [`StateNode`] in the requested format.
///
/// Both formats come from the same resolved tree, so the element count is
/// exact regardless of newlines inside names/values.
fn render_tree(format: &SnapshotFormat, tree: &StateNode) -> String {
    match format {
        SnapshotFormat::Json => serde_json::to_string_pretty(tree).unwrap_or_default(),
        SnapshotFormat::Tree => {
            let mut out = String::new();
            write_outline(tree, 0, &mut out);
            out
        }
    }
}

/// Indented one-line-per-element outline (role, name, value, non-default
/// states).
fn write_outline(node: &StateNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push_str(&node.role);
    if let Some(name) = node.name.as_deref().filter(|n| !n.is_empty()) {
        out.push_str(&format!(" name={}", escape_text(name)));
    }
    if let Some(value) = node.value.as_deref().filter(|v| !v.is_empty()) {
        out.push_str(&format!(" value={}", escape_text(value)));
    }
    for token in node.states.outline_tokens() {
        out.push(' ');
        out.push_str(token);
    }
    out.push('\n');
    for child in &node.children {
        write_outline(child, depth + 1, out);
    }
}

/// Quote a name/value for the outline, flattening newlines so one element
/// always renders as exactly one line.
fn escape_text(text: &str) -> String {
    format!("{text:?}").replace('\n', " ")
}

/// Count nodes and reject snapshots beyond the [`MAX_NODES`] budget so a
/// runaway tree fails cleanly instead of flooding the model's context.
fn checked_node_count(node: &StateNode) -> Result<usize, String> {
    let count = count_nodes(node);
    if count > MAX_NODES {
        return Err(format!(
            "computer_snapshot: {count} elements exceed the {MAX_NODES}-node budget; \
             narrow the snapshot with `selector` or lower `max_depth`"
        ));
    }
    Ok(count)
}

fn count_nodes(node: &StateNode) -> usize {
    let mut total = 1;
    for child in &node.children {
        total += count_nodes(child);
    }
    total
}

/// Shell-surface dispatch tests — same mock fixture as the wait tests
/// (Taskbar surface at `MOCK_SHELL_PID`), driving `snapshot_surface`
/// directly through its provider seam.
#[cfg(test)]
mod snapshot_surface_tests {
    use std::time::Duration;

    use xa11y::{ShellSurface, ShellSurfaceKind, mock};

    use super::super::types::{ComputerSnapshot, SnapshotFormat};
    use super::{snapshot_surface, DEFAULT_MAX_DEPTH};

    fn taskbar() -> xa11y::ShellSurface {
        ShellSurface::by_kind_with(mock::build_provider(), ShellSurfaceKind::Taskbar, Duration::ZERO)
            .expect("mock fixture carries a taskbar")
    }

    /// A surface-rooted snapshot without a selector outlines the WHOLE
    /// surface and reports the surface's own identity (name + shell pid),
    /// not an app's.
    #[test]
    fn surface_root_reports_surface_identity() {
        let input = ComputerSnapshot::default();
        let out = snapshot_surface(&input, taskbar(), Some(DEFAULT_MAX_DEPTH as usize), SnapshotFormat::default())
            .expect("mock taskbar must snapshot");
        assert_eq!(out.app, "Taskbar");
        assert_eq!(out.pid, Some(mock::MOCK_SHELL_PID));
        assert!(out.elements >= 1);
        assert!(out.snapshot.contains("Taskbar"), "{}", out.snapshot);
    }

    /// With a selector the SAME locator flow narrows into the surface's
    /// subtree — `nth` (1-based) applies exactly like the app path.
    #[test]
    fn surface_selector_narrows_like_app_path() {
        let input = ComputerSnapshot {
            selector: Some("button".into()),
            nth: Some(1),
            ..ComputerSnapshot::default()
        };
        let out = snapshot_surface(&input, taskbar(), Some(DEFAULT_MAX_DEPTH as usize), SnapshotFormat::default())
            .expect("mock taskbar carries buttons");
        assert_eq!(out.app, "Taskbar");
        assert_eq!(out.elements, 1, "nth(1) picks exactly one match");
    }

    /// A selector that matches nothing under the surface is an honest
    /// error naming the surface — same contract as the app path.
    #[test]
    fn surface_selector_miss_is_honest_error() {
        let input = ComputerSnapshot {
            selector: Some("text_field[name='definitely-not-here']".into()),
            ..ComputerSnapshot::default()
        };
        let err = snapshot_surface(&input, taskbar(), Some(DEFAULT_MAX_DEPTH as usize), SnapshotFormat::default())
            .expect_err("missing selector must fail");
        assert!(!err.is_empty());
    }
}

#[cfg(test)]
mod snapshot_render_tests {
    use super::super::types::{ElementStates, StateNode, ToggleState};
    use super::{render_tree, write_outline, SnapshotFormat};

    /// A node with only default states renders as bare `role name=value` —
    /// no state tokens, keeping the common case one short line.
    #[test]
    fn default_states_render_nothing() {
        let node = StateNode {
            role: "button".into(),
            name: Some("OK".into()),
            value: None,
            states: ElementStates::default(),
            children: vec![],
        };
        assert_eq!(write_outline_str(&node), "button name=\"OK\"\n");
    }

    /// Non-default flags render as aria-vocabulary tokens, in fixed order:
    /// disabled, hidden, focused, checked, selected, expanded, editable, busy.
    #[test]
    fn non_default_states_render_as_tokens_in_fixed_order() {
        let states = ElementStates {
            enabled: false,
            visible: false,
            focused: true,
            checked: Some(ToggleState::On),
            selected: true,
            expanded: Some(false),
            editable: true,
            busy: true,
        };
        let node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states,
            children: vec![],
        };
        assert_eq!(
            write_outline_str(&node),
            "checkbox disabled hidden focused checked selected collapsed editable busy\n"
        );
    }

    /// An unchecked checkbox is meaningful (the user wants to know it is
    /// checkable and off), so `Some(Off)` renders `unchecked` — unlike the
    /// absence of checkability (`None`), which renders nothing.
    #[test]
    fn unchecked_is_reported_but_non_checkable_is_silent() {
        let unchecked = ElementStates {
            checked: Some(ToggleState::Off),
            ..ElementStates::default()
        };
        let unchecked_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: unchecked,
            children: vec![],
        };
        assert_eq!(write_outline_str(&unchecked_node), "checkbox unchecked\n");

        let mixed = ElementStates {
            checked: Some(ToggleState::Mixed),
            ..ElementStates::default()
        };
        let mixed_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: mixed,
            children: vec![],
        };
        assert_eq!(write_outline_str(&mixed_node), "checkbox mixed\n");
    }

    /// `json` format serializes the node directly: the flat `states` object
    /// is present with the normalized subset, `checked` is "off" (lowercase
    /// enum) and children nest recursively.
    #[test]
    fn json_format_embeds_states_object() {
        let child = StateNode {
            role: "text_field".into(),
            name: Some("Search".into()),
            value: Some("hi".into()),
            states: ElementStates {
                editable: true,
                ..ElementStates::default()
            },
            children: vec![],
        };
        let root = StateNode {
            role: "window".into(),
            name: Some("Main".into()),
            value: None,
            states: ElementStates::default(),
            children: vec![child],
        };
        let json = render_tree(&SnapshotFormat::Json, &root);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["role"], "window");
        assert_eq!(value["states"]["enabled"], true);
        assert_eq!(value["states"]["visible"], true);
        assert_eq!(value["states"]["focused"], false);
        // `.get()` (not indexing) so a MISSING key would fail: indexing an
        // absent key also yields Null, which would make this assertion pass
        // even if `checked` were dropped from the serialization entirely.
        assert_eq!(value["states"].get("checked"), Some(&serde_json::Value::Null));
        assert_eq!(value["states"].get("expanded"), Some(&serde_json::Value::Null));
        let grandchild = &value["children"][0];
        assert_eq!(grandchild["states"]["editable"], true);
        assert_eq!(grandchild["states"].get("checked"), Some(&serde_json::Value::Null));
        // ToggleState serializes lowercase.
        let toggled = ElementStates {
            checked: Some(ToggleState::On),
            ..ElementStates::default()
        };
        let toggled_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: toggled,
            children: vec![],
        };
        let toggled_json = render_tree(&SnapshotFormat::Json, &toggled_node);
        let toggled_value: serde_json::Value =
            serde_json::from_str(&toggled_json).expect("valid json");
        assert_eq!(toggled_value["states"]["checked"], "on");
        let mixed_json_value = ElementStates {
            checked: Some(ToggleState::Mixed),
            ..ElementStates::default()
        };
        let mixed_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: mixed_json_value,
            children: vec![],
        };
        let mixed_json = render_tree(&SnapshotFormat::Json, &mixed_node);
        let mixed_value: serde_json::Value = serde_json::from_str(&mixed_json).expect("valid json");
        assert_eq!(mixed_value["states"]["checked"], "mixed");
    }

    fn write_outline_str(node: &StateNode) -> String {
        let mut out = String::new();
        write_outline(node, 0, &mut out);
        out
    }
}
