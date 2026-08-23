//! `lsp_definitions` engine: hybrid addressing, fan-out across servers.

use cosh_sdk::lsp::lsp_types::{
    GotoDefinitionResponse, request::GotoDefinition, request::Request as _,
};

use super::{
    support,
    types::{DefinitionsInput, DefinitionsOutput, LocationEntry},
};
use cosh_sdk::lsp::uri_to_path;

/// Resolve `textDocument/definition` on the first capable server that returns
/// locations. Null answers fall through to the next server.
pub async fn run_definitions(
    deps: &super::support::Deps<'_>,
    input: &DefinitionsInput,
) -> Result<DefinitionsOutput, String> {
    let path = deps.manager.root().join(&input.location.file_path);
    let (position, clients) = support::resolve_target(
        deps,
        &path,
        &input.location.position,
        &input.location.symbol,
    )
    .await?;

    let params = serde_json::json!({
        "textDocument": { "uri": support::uri(&path)? },
        "position": position,
    });

    let response = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| {
            caps.definition_provider
                .as_ref()
                .is_some_and(|provider| match provider {
                    cosh_sdk::lsp::lsp_types::OneOf::Left(enabled) => *enabled,
                    cosh_sdk::lsp::lsp_types::OneOf::Right(_) => true,
                })
        },
        |_client| (GotoDefinition::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let entries = response.map_or_else(Vec::new, locations_to_entries);

    Ok(DefinitionsOutput {
        formatted: render(&entries),
        definitions: entries,
    })
}

fn locations_to_entries(response: GotoDefinitionResponse) -> Vec<LocationEntry> {
    let mut locations: Vec<cosh_sdk::lsp::lsp_types::Location> = match response {
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|link| cosh_sdk::lsp::lsp_types::Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
    };
    locations.sort_by_key(|location| location.range.start);
    locations
        .iter()
        .filter_map(|location| {
            let path = uri_to_path(&location.uri)?;
            let text = std::fs::read_to_string(&path).ok()?;
            let line = text.lines().nth(location.range.start.line as usize)?;
            Some(LocationEntry {
                path: path.display().to_string(),
                line: location.range.start.line + 1,
                character: location.range.start.character + 1,
                text: line.trim().to_owned(),
            })
        })
        .collect()
}

fn render(entries: &[LocationEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{}:{}  {}",
                entry.path, entry.line, entry.character, entry.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
