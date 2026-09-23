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
pub(crate) const DEFAULT_MAX_DEPTH: u32 = 12;
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
pub(crate) fn snapshot_surface(
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
pub(crate) fn render_tree(format: &SnapshotFormat, tree: &StateNode) -> String {
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
pub(crate) fn write_outline(node: &StateNode, depth: usize, out: &mut String) {
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

