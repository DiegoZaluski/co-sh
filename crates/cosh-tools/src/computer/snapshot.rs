//! `computer_snapshot` — capture an application's accessibility tree.
//!
//! Two renderings share one resolution path: the compact indented outline
//! ([`xa11y::App::dump`], the format the xa11y docs recommend for figuring
//! out roles/names before writing selectors) and structured JSON
//! ([`xa11y::TreeNode`] serialized directly).
//!
//! Resolution is fail-fast: only the application lookup auto-waits (up to
//! `timeout_ms`). Selector matching resolves once — a selector on an
//! element that is not yet in the tree fails immediately; re-call after
//! the interface settles instead of expecting the tool to poll.
use std::time::Duration;

use xa11y::{App, AppExt, TreeNode};

use super::types::{SnapshotFormat, SnapshotOutput, ComputerSnapshot};

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
    let has_name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();
    if has_name && input.pid.is_some() {
        return Err("computer_snapshot: provide `name` or `pid`, not both".into());
    }
    if !has_name && input.pid.is_none() {
        return Err("computer_snapshot: provide `name` or `pid`".into());
    }
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
    let name = input.name.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let app = match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout),
        (None, Some(pid)) => App::by_pid(pid, timeout),
        _ => unreachable!("validated in snapshot"),
    }
    .map_err(|e| format!("computer_snapshot: resolve application: {e}"))?;

    let max_depth = Some(
        input
            .max_depth
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .min(MAX_MAX_DEPTH) as usize,
    );
    let format = input.format.unwrap_or_default();

    let (snapshot, elements) = match &input.selector {
        Some(selector) => {
            let nth = input.nth.unwrap_or(1);
            let locator = app.locator(selector).nth(nth);
            render(&format, &locator, max_depth)?
        }
        None => render_app(&format, &app, max_depth)?,
    };

    Ok(SnapshotOutput {
        app: app.name.clone(),
        pid: app.pid,
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
    let tree = locator
        .tree(max_depth)
        .map_err(|e| format!("computer_snapshot: {e}"))?;
    let count = checked_node_count(&tree)?;
    Ok((render_tree(format, &tree), count))
}

/// Render an application-rooted snapshot in the requested format.
fn render_app(
    format: &SnapshotFormat,
    app: &App,
    max_depth: Option<usize>,
) -> Result<(String, usize), String> {
    let tree = app.tree(max_depth).map_err(|e| format!("computer_snapshot: {e}"))?;
    let count = checked_node_count(&tree)?;
    Ok((render_tree(format, &tree), count))
}

/// Serialize a [`TreeNode`] in the requested format.
///
/// Both formats come from the same resolved tree, so the element count is
/// exact regardless of newlines inside names/values.
fn render_tree(format: &SnapshotFormat, tree: &TreeNode) -> String {
    match format {
        SnapshotFormat::Json => serde_json::to_string_pretty(tree).unwrap_or_default(),
        SnapshotFormat::Tree => {
            let mut out = String::new();
            write_outline(tree, 0, &mut out);
            out
        }
    }
}

/// Indented one-line-per-element outline (role, name, value).
fn write_outline(node: &TreeNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push_str(&node.role);
    if let Some(name) = node.name.as_deref().filter(|n| !n.is_empty()) {
        out.push_str(&format!(" name={}", escape_text(name)));
    }
    if let Some(value) = node.value.as_deref().filter(|v| !v.is_empty()) {
        out.push_str(&format!(" value={}", escape_text(value)));
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
fn checked_node_count(node: &TreeNode) -> Result<usize, String> {
    let count = count_nodes(node);
    if count > MAX_NODES {
        return Err(format!(
            "computer_snapshot: {count} elements exceed the {MAX_NODES}-node budget; \
             narrow the snapshot with `selector` or lower `max_depth`"
        ));
    }
    Ok(count)
}

fn count_nodes(node: &TreeNode) -> usize {
    let mut total = 1;
    for child in &node.children {
        total += count_nodes(child);
    }
    total
}
