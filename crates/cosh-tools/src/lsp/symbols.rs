//! `lsp_symbols` engine: hierarchical document symbols, flattened for models.

use cosh_sdk::lsp::lsp_types::request::{DocumentSymbolRequest, Request as _};
use cosh_sdk::lsp::lsp_types::{DocumentSymbol, DocumentSymbolResponse, SymbolKind};
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
                kind: kind_name(node.kind),
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

/// Common LSP symbol kinds; unknown numbers degrade to a numeric label
/// instead of being dropped (dropping would silently hide declarations).
fn kind_name(kind: SymbolKind) -> String {
    // SymbolKind is a transparent newtype over i32 in lsp-types 0.97 with
    // associated consts for every spec kind.
    const FILE: SymbolKind = SymbolKind::FILE;
    const MODULE: SymbolKind = SymbolKind::MODULE;
    const NAMESPACE: SymbolKind = SymbolKind::NAMESPACE;
    const PACKAGE: SymbolKind = SymbolKind::PACKAGE;
    const CLASS: SymbolKind = SymbolKind::CLASS;
    const METHOD: SymbolKind = SymbolKind::METHOD;
    const PROPERTY: SymbolKind = SymbolKind::PROPERTY;
    const FIELD: SymbolKind = SymbolKind::FIELD;
    const CONSTRUCTOR: SymbolKind = SymbolKind::CONSTRUCTOR;
    const ENUM: SymbolKind = SymbolKind::ENUM;
    const INTERFACE: SymbolKind = SymbolKind::INTERFACE;
    const FUNCTION: SymbolKind = SymbolKind::FUNCTION;
    const VARIABLE: SymbolKind = SymbolKind::VARIABLE;
    const CONSTANT: SymbolKind = SymbolKind::CONSTANT;
    const STRING: SymbolKind = SymbolKind::STRING;
    const NUMBER: SymbolKind = SymbolKind::NUMBER;
    const BOOLEAN: SymbolKind = SymbolKind::BOOLEAN;
    const ARRAY: SymbolKind = SymbolKind::ARRAY;
    const OBJECT: SymbolKind = SymbolKind::OBJECT;
    const KEY: SymbolKind = SymbolKind::KEY;
    const NULL: SymbolKind = SymbolKind::NULL;
    const ENUM_MEMBER: SymbolKind = SymbolKind::ENUM_MEMBER;
    const STRUCT: SymbolKind = SymbolKind::STRUCT;
    const EVENT: SymbolKind = SymbolKind::EVENT;
    const OPERATOR: SymbolKind = SymbolKind::OPERATOR;
    const TYPE_PARAMETER: SymbolKind = SymbolKind::TYPE_PARAMETER;

    match kind {
        FILE => "file".into(),
        MODULE | NAMESPACE | PACKAGE => "module".into(),
        CLASS => "class".into(),
        METHOD => "method".into(),
        PROPERTY => "property".into(),
        FIELD => "field".into(),
        CONSTRUCTOR => "constructor".into(),
        ENUM => "enum".into(),
        INTERFACE => "interface".into(),
        FUNCTION => "function".into(),
        VARIABLE => "variable".into(),
        CONSTANT => "constant".into(),
        STRING | NUMBER | BOOLEAN | ARRAY | OBJECT | KEY | NULL => "literal".into(),
        ENUM_MEMBER => "enum-member".into(),
        STRUCT => "struct".into(),
        EVENT => "event".into(),
        OPERATOR => "operator".into(),
        TYPE_PARAMETER => "type-parameter".into(),
        // Unrecognized kinds still show up — silently hiding declarations
        // would be worse than a generic label.
        _ => "symbol".into(),
    }
}
