//! `lsp_references` engine: hybrid addressing, grouped output.

use std::collections::BTreeMap;

use cosh_sdk::lsp::lsp_types::request::{References, Request as _};
use serde_json::json;

use super::{
    support,
    types::{ReferenceFile, ReferencesInput, ReferencesOutput},
};

/// Resolve `textDocument/references` on the first capable server that answers.
pub async fn run_references(
    deps: &super::support::Deps<'_>,
    input: &ReferencesInput,
) -> Result<ReferencesOutput, String> {
    let path = deps.manager.root().join(&input.location.file_path);
    let (position, clients) = support::resolve_target(
        deps,
        &path,
        &input.location.position,
        &input.location.symbol,
    )
    .await?;

    let include_declaration = input.include_declaration.unwrap_or(true);
    let max_items = usize::try_from(input.max_items.unwrap_or(100).max(1)).unwrap_or(100);

    let params = json!({
        "textDocument": { "uri": support::uri(&path)? },
        "position": position,
        "context": { "includeDeclaration": include_declaration },
    });

    let response: Option<Vec<cosh_sdk::lsp::lsp_types::Location>> = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| super::support::provider_enabled(&caps.references_provider),
        |_client| (References::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let locations: Vec<cosh_sdk::lsp::lsp_types::Location> = response.unwrap_or_default();

    // Count BEFORE the cap so the model learns the true blast radius; the
    // marker mirrors diagnostics' `… and N more` honesty.
    let mut by_file: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    let mut skipped_unresolvable = 0usize;
    for location in &locations {
        match cosh_sdk::lsp::uri_to_path(&location.uri) {
            Some(entry_path) => {
                by_file
                    .entry(entry_path.display().to_string())
                    .or_default()
                    .push((
                        location.range.start.line + 1,
                        location.range.start.character + 1,
                    ));
            }
            None => skipped_unresolvable += 1,
        }
    }

    let total: usize = by_file.values().map(|entries| entries.len()).sum();
    let mut files: Vec<ReferenceFile> = Vec::new();
    let mut shown = 0usize;
    'outer: for (file_path, mut locations) in by_file {
        if shown >= max_items {
            break;
        }
        let remaining = max_items - shown;
        if locations.len() <= remaining {
            shown += locations.len();
            files.push(ReferenceFile {
                path: file_path,
                locations,
            });
        } else {
            locations.truncate(remaining);
            shown += remaining;
            files.push(ReferenceFile {
                path: file_path,
                locations,
            });
            break 'outer;
        }
    }

    let hidden = total.saturating_sub(shown);
    let formatted = render(&files, total, hidden, skipped_unresolvable);
    Ok(ReferencesOutput {
        total,
        files,
        formatted,
    })
}

fn render(files: &[ReferenceFile], total: usize, hidden: usize, skipped: usize) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("{total} reference(s):")];
    for file in files {
        for (line, character) in &file.locations {
            lines.push(format!("{}:{line}:{character}", file.path));
        }
    }
    if hidden > 0 {
        lines.push(format!("… and {hidden} more references"));
    }
    if skipped > 0 {
        lines.push(format!(
            "({skipped} location(s) with non-file URIs were skipped)"
        ));
    }
    lines.join("\n")
}
