//! `lsp_workspace_symbols` engine: project-wide symbol search.

use cosh_sdk::lsp::lsp_types::{
    WorkspaceSymbol,
    request::{Request as _, WorkspaceSymbolRequest},
};
use serde_json::json;

use super::{
    support,
    types::{WorkspaceSymbolEntry, WorkspaceSymbolsInput, WorkspaceSymbolsOutput},
};
use cosh_sdk::lsp::uri_to_path;

/// Resolve `workspace/symbol` on the first capable running server.
pub async fn run_workspace_symbols(
    deps: &super::support::Deps<'_>,
    input: &WorkspaceSymbolsInput,
) -> Result<WorkspaceSymbolsOutput, String> {
    let clients = deps.manager.running_clients();
    if clients.is_empty() {
        return Err(
            "no language servers are running yet — touch a file first (read/edit) \
             to start them, then retry"
                .into(),
        );
    }

    let query = input.query.clone().unwrap_or_default();
    let max_items = usize::try_from(input.max_items.unwrap_or(100).max(1)).unwrap_or(100);

    let params = json!({ "query": query });

    let response: Option<Vec<WorkspaceSymbol>> = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| {
            caps.workspace_symbol_provider
                .as_ref()
                .is_some_and(|provider| match provider {
                    cosh_sdk::lsp::lsp_types::OneOf::Left(enabled) => *enabled,
                    cosh_sdk::lsp::lsp_types::OneOf::Right(_) => true,
                })
        },
        |_client| (WorkspaceSymbolRequest::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let mut symbols = Vec::new();
    for symbol in response.unwrap_or_default() {
        if symbols.len() >= max_items {
            break;
        }
        // WorkspaceSymbol.location is OneOf<Location, {uri}> in 3.17; both
        // carry the uri we need for the path.
        let (uri, line, character) = match &symbol.location {
            cosh_sdk::lsp::lsp_types::OneOf::Left(location) => (
                location.uri.clone(),
                location.range.start.line + 1,
                location.range.start.character + 1,
            ),
            cosh_sdk::lsp::lsp_types::OneOf::Right(bare) => (bare.uri.clone(), 1, 1),
        };
        let Some(path) = uri_to_path(&uri) else {
            continue;
        };

        symbols.push(WorkspaceSymbolEntry {
            name: symbol.name.clone(),
            kind: support::symbol_kind_name(symbol.kind),
            path: path.display().to_string(),
            line,
            character,
        });
    }

    let total = symbols.len();
    Ok(WorkspaceSymbolsOutput {
        formatted: render(&symbols),
        total,
        symbols,
    })
}

fn render(symbols: &[WorkspaceSymbolEntry]) -> String {
    symbols
        .iter()
        .map(|entry| {
            format!(
                "{} {}  {}:{}:{}",
                entry.kind, entry.name, entry.path, entry.line, entry.character
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
