//! `lsp_call_hierarchy` engine: incoming/outgoing calls.

use cosh_sdk::lsp::lsp_types::{
    CallHierarchyItem,
    request::{
        CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare, Request as _,
    },
};
use serde_json::json;

use super::{
    support,
    types::{CallEntry, CallHierarchyInput, CallHierarchyOutput},
};

/// Resolve the call hierarchy around a symbol.
pub async fn run_call_hierarchy(
    deps: &super::support::Deps<'_>,
    input: &CallHierarchyInput,
) -> Result<CallHierarchyOutput, String> {
    let direction = match input.direction.as_deref() {
        None | Some("outgoing") => "outgoing",
        Some("incoming") => "incoming",
        Some(other) => {
            return Err(format!(
                "invalid direction `{other}`: expected \"incoming\" or \"outgoing\""
            ));
        }
    };
    let max_items = usize::try_from(input.max_items.unwrap_or(50).max(1)).unwrap_or(50);

    let path = deps.manager.root().join(&input.file_path);
    let (position, clients) =
        support::resolve_target(deps, &path, &input.position, &input.symbol).await?;

    // Phase 1: prepare the hierarchy item at this position.
    let prepare_params = json!({
        "textDocument": { "uri": support::uri(&path)? },
        "position": position,
    });
    let prepared: Option<Vec<CallHierarchyItem>> = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| caps.call_hierarchy_provider.is_some(),
        |_client| {
            (
                CallHierarchyPrepare::METHOD.to_owned(),
                prepare_params.clone(),
            )
        },
    )
    .await?;

    let item = prepared
        .and_then(|items| items.into_iter().next())
        .ok_or_else(|| "no call hierarchy item at this position".to_owned())?;

    // Phase 2: walk in the requested direction.
    let calls: Vec<CallEntry> = match direction {
        "outgoing" => {
            let params = json!({ "item": item });
            let response: Option<Vec<cosh_sdk::lsp::lsp_types::CallHierarchyOutgoingCall>> =
                super::support::first_answer(
                    &clients,
                    deps.request_timeout,
                    capability,
                    |_client| {
                        (
                            CallHierarchyOutgoingCalls::METHOD.to_owned(),
                            params.clone(),
                        )
                    },
                )
                .await?;
            let unwrapped = response.unwrap_or_default();
            unwrapped
                .into_iter()
                .take(max_items)
                .map(|call| entry_from(call.to, Vec::new()))
                .collect()
        }
        _ => {
            let params = json!({ "item": item });
            let response: Option<Vec<cosh_sdk::lsp::lsp_types::CallHierarchyIncomingCall>> =
                super::support::first_answer(
                    &clients,
                    deps.request_timeout,
                    capability,
                    |_client| {
                        (
                            CallHierarchyIncomingCalls::METHOD.to_owned(),
                            params.clone(),
                        )
                    },
                )
                .await?;
            let unwrapped = response.unwrap_or_default();
            unwrapped
                .into_iter()
                .take(max_items)
                .map(|call| {
                    let sites = call
                        .from_ranges
                        .iter()
                        .map(|range| (range.start.line + 1, range.start.character + 1))
                        .collect();
                    entry_from(call.from, sites)
                })
                .collect()
        }
    };

    Ok(CallHierarchyOutput {
        direction: direction.to_owned(),
        formatted: render(direction, &calls),
        calls,
    })
}

fn capability(caps: &cosh_sdk::lsp::lsp_types::ServerCapabilities) -> bool {
    caps.call_hierarchy_provider.is_some()
}

fn entry_from(item: CallHierarchyItem, sites: Vec<(u32, u32)>) -> CallEntry {
    let kind = support::symbol_kind_name(item.kind);
    let path = cosh_sdk::lsp::uri_to_path(&item.uri)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| item.uri.to_string());
    CallEntry {
        name: item.name,
        kind,
        path,
        line: item.range.start.line + 1,
        character: item.range.start.character + 1,
        sites,
    }
}

fn render(direction: &str, calls: &[CallEntry]) -> String {
    if calls.is_empty() {
        return format!("No {direction} calls.");
    }
    let label = if direction == "outgoing" {
        "calls →"
    } else {
        "← called by"
    };
    calls
        .iter()
        .map(
            |entry| match (&entry.sites.is_empty(), entry.sites.first()) {
                (false, Some((line, character))) => {
                    format!(
                        "{} {} {}:{}:{} ({label} site {line}:{character})",
                        entry.kind, entry.name, entry.path, entry.line, entry.character
                    )
                }
                _ => format!(
                    "{} {} {}:{}",
                    entry.kind, entry.name, entry.path, entry.line
                ),
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
}
