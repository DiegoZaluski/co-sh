//! `lsp_diagnostics` engine: ensure servers, settle, snapshot and render.

use std::time::Duration;

use super::{support::Deps, types::DiagnosticsInput, types::DiagnosticsOutput};
use cosh_sdk::lsp::SeverityFilter;

/// Resolve the severity filter from the model-facing string.
fn parse_severity(input: &DiagnosticsInput) -> Result<SeverityFilter, String> {
    match input.severity.as_deref() {
        None | Some("all") => Ok(SeverityFilter::All),
        Some("errors") => Ok(SeverityFilter::ErrorsOnly),
        Some("warnings") => Ok(SeverityFilter::WarningAndUp),
        Some(other) => Err(format!(
            "invalid severity `{other}`: expected \"errors\", \"warnings\" or \"all\""
        )),
    }
}

/// Ensure the file's servers are up, wait for diagnostics to settle, then
/// render. With `file_path` omitted this reports a workspace-wide snapshot of
/// current state — nothing is opened or waited on.
pub async fn run_diagnostics(
    deps: &Deps<'_>,
    input: &DiagnosticsInput,
) -> Result<DiagnosticsOutput, String> {
    let filter = parse_severity(input)?;
    let max_items = usize::try_from(input.max_items.unwrap_or(50).max(1)).unwrap_or(50);
    let cap = Duration::from_millis(u64::from(input.settle_ms.unwrap_or(5_000)));

    let diags = match &input.file_path {
        Some(file_path) => {
            let path = deps.manager.root().join(file_path);
            super::support::prepare(deps, &path).await?;
            let settled = deps.diagnostics.wait_for_settle(cap).await;
            return Ok(finish(deps.diagnostics, &path, filter, max_items, settled));
        }
        None => deps
            .diagnostics
            .snapshot_all(Some(deps.manager.root()))
            .into_iter()
            .flat_map(|(_, per_file)| per_file)
            .collect::<Vec<_>>(),
    };

    // Workspace-wide: report current knowledge; no churn was triggered, so
    // the snapshot is as fresh as the push stream allows.
    let formatted = cosh_sdk::lsp::format_for_model(&diags, filter, max_items);
    Ok(DiagnosticsOutput {
        count: formatted.lines().count(),
        formatted,
        settled: true,
    })
}

fn finish(
    engine: &crate::lsp::DiagnosticsEngine,
    path: &std::path::Path,
    filter: SeverityFilter,
    max_items: usize,
    settled: bool,
) -> DiagnosticsOutput {
    let snapshot = engine.snapshot_for(path);
    let formatted = cosh_sdk::lsp::format_for_model(&snapshot, filter, max_items);
    DiagnosticsOutput {
        count: formatted.lines().count(),
        formatted,
        settled,
    }
}
