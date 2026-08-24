//! `lsp_symbols` engine: hierarchical document symbols, flattened for models.

use cosh_sdk::lsp::lsp_types::request::{DocumentSymbolRequest, Request as _};
use cosh_sdk::lsp::lsp_types::{DocumentSymbol, DocumentSymbolResponse};
use serde_json::json;

use super::{
    support,
    types::{SymbolEntry, SymbolsInput, SymbolsOutput},
};

/// Resolve `textDocument/documentSymbol`, optionally filtering by substring
/// and capping the result.
pub async fn run_symbols(
    deps: &super::support::Deps<'_>,
    input: &SymbolsInput,
) -> Result<SymbolsOutput, String> {
    let path = deps.manager.root().join(&input.file_path);
    let _clients = support::prepare(deps, &path).await?;

    let params = json!({
        "textDocument": { "uri": support::uri(&path)? },
    });

    let response: Option<DocumentSymbolResponse> = super::support::first_answer(
        &_clients,
        deps.request_timeout,
        |caps| caps.document_symbol_provider.is_some(),
        |_client| (DocumentSymbolRequest::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let query = input.query.as_deref().map(str::to_lowercase);
    let max_items = usize::try_from(input.max_items.unwrap_or(200).max(1)).unwrap_or(200);

    let mut symbols = Vec::new();
    if let Some(DocumentSymbolResponse::Nested(roots)) = response {
        walk(&roots, &None, &query, max_items, &mut symbols);
    }

    Ok(SymbolsOutput {
        formatted: render(&symbols),
        symbols,
    })
}

fn walk(
    nodes: &[DocumentSymbol],
    container: &Option<String>,
    query: &Option<String>,
    max_items: usize,
    out: &mut Vec<SymbolEntry>,
) {
    for node in nodes {
        if out.len() >= max_items {
            return;
        }
        let matches_query = query
            .as_ref()
            .is_none_or(|needle| node.name.to_lowercase().contains(needle));
        if matches_query {
            out.push(SymbolEntry {
                name: node.name.clone(),
                kind: super::support::symbol_kind_name(node.kind),
                line: node.range.start.line + 1,
                character: node.range.start.character + 1,
                container: container.clone(),
            });
        }
        if let Some(children) = &node.children {
            let enclosing = Some(node.name.clone());
            walk(children, &enclosing, query, max_items, out);
        }
    }
}

/// Indentation preserves hierarchy; containers prefix their children when the
/// server did not already fill `container`.
fn render(symbols: &[SymbolEntry]) -> String {
    symbols
        .iter()
        .map(|entry| match &entry.container {
            Some(container) => format!(
                "{container}::{} {}:{}:{}",
                entry.kind, entry.name, entry.line, entry.character
            ),
            None => format!(
                "{} {}:{}:{}",
                entry.kind, entry.name, entry.line, entry.character
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
