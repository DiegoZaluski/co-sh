//! `lsp_code_actions` engine: quickfixes and refactoring suggestions.
//!
//! The flow that makes this valuable for agents:
//!
//! 1. Model writes code → passive injection reports an error
//! 2. Model calls `lsp_code_actions(file, line)` → gets back available fixes
//! 3. Model re-calls with `apply_index` to apply the chosen fix to disk
//!
//! Without this, the model would need to manually figure out the fix from
//! the diagnostic message — slower and more error-prone.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use cosh_sdk::lsp::{
    LanguageServer, PositionEncoding,
    lsp_types::{
        CodeActionOrCommand, Position, WorkspaceEdit,
        request::{CodeActionRequest as CodeActionReq, Request as _},
    },
};
use serde_json::json;

use super::{
    support,
    types::{CodeActionEntry, CodeActionsInput, CodeActionsOutput},
};

/// List or apply code actions at a position.
pub async fn run_code_actions(
    deps: &super::support::Deps<'_>,
    input: &CodeActionsInput,
) -> Result<CodeActionsOutput, String> {
    let path = deps.manager.root().join(&input.file_path);
    let clients = support::prepare(deps, &path).await?;
    if clients.is_empty() {
        return Err(format!("no language server handles `{}`", path.display()));
    }
    let encoding = clients[0].position_encoding();

    let line_idx = input.line.saturating_sub(1);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("cannot read `{}`: {err}", path.display()))?;
    let line_len = text
        .lines()
        .nth(line_idx as usize)
        .map(|l| l.len() as u32)
        .unwrap_or(0);

    let range = cosh_sdk::lsp::lsp_types::Range {
        start: Position::new(line_idx, 0),
        end: Position::new(line_idx, line_len),
    };

    // Build context from stored diagnostics overlapping the target line.
    let stored = deps.diagnostics.snapshot_for(&path);
    let matching: Vec<_> = stored
        .iter()
        .filter(|d| d.range.start.line <= line_idx && d.range.end.line >= line_idx)
        .cloned()
        .collect();

    let params = json!({
        "textDocument": { "uri": support::uri(&path)? },
        "range": range,
        "context": {
            "diagnostics": matching,
            "reason": "implicit",
        },
    });

    let raw = fetch_raw_actions(&clients, deps.request_timeout, &params).await?;
    let actions = flatten(&raw);

    match input.apply_index {
        None => Ok(CodeActionsOutput {
            applied: false,
            formatted: render_list(&actions),
            actions,
        }),
        Some(index) => {
            // Use the RAW action to access its edit payload.
            let raw_action = raw
                .get(index)
                .ok_or_else(|| format!("action index {index} out of range"))?;

            let (title, workspace_edit) = match raw_action {
                CodeActionOrCommand::CodeAction(action) => {
                    let edit = action.edit.as_ref().ok_or_else(|| {
                        format!(
                            "action `{}` carries a command, not an edit — cannot auto-apply",
                            action.title
                        )
                    })?;
                    (&action.title, edit)
                }
                CodeActionOrCommand::Command(_) => {
                    return Err("commands cannot be auto-applied".into());
                }
            };

            apply_workspace_edit(workspace_edit, encoding)?;
            for client in &clients {
                let _ = client.touch_file(&path).await;
            }

            Ok(CodeActionsOutput {
                applied: true,
                actions: vec![],
                formatted: format!("Applied: {title}"),
            })
        }
    }
}

/// Fetch raw code actions from the first capable server.
async fn fetch_raw_actions(
    clients: &[Arc<LanguageServer>],
    timeout: Duration,
    params: &serde_json::Value,
) -> Result<Vec<CodeActionOrCommand>, String> {
    let response: Option<Vec<CodeActionOrCommand>> =
        support::first_answer(clients, timeout, capability, |_client| {
            (CodeActionReq::METHOD.to_owned(), params.clone())
        })
        .await?;
    Ok(response.unwrap_or_default())
}

fn capability(caps: &cosh_sdk::lsp::lsp_types::ServerCapabilities) -> bool {
    caps.code_action_provider.is_some()
}

/// Flatten raw items into typed entries with file info.
fn flatten(raw: &[CodeActionOrCommand]) -> Vec<CodeActionEntry> {
    raw.iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            CodeActionOrCommand::CodeAction(action) => {
                let files = extract_edit_files(action.edit.as_ref());
                Some(CodeActionEntry {
                    index,
                    title: action.title.clone(),
                    kind: action.kind.as_ref().map(|k| k.as_str().to_owned()),
                    has_edit: action.edit.is_some(),
                    files,
                })
            }
            CodeActionOrCommand::Command(_) => None,
        })
        .collect()
}

fn extract_edit_files(edit: Option<&WorkspaceEdit>) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(edit) = edit
        && let Some(changes) = &edit.changes
    {
        for uri in changes.keys() {
            if let Some(path) = cosh_sdk::lsp::uri_to_path(uri) {
                files.push(path.display().to_string());
            }
        }
    }
    files
}

/// Apply a WorkspaceEdit to disk using the shared splicing logic.
fn apply_workspace_edit(edit: &WorkspaceEdit, encoding: PositionEncoding) -> Result<(), String> {
    let mut per_file: BTreeMap<PathBuf, Vec<super::support::PlannedEdit>> = BTreeMap::new();

    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            let Some(path) = cosh_sdk::lsp::uri_to_path(uri) else {
                continue;
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|err| format!("cannot read `{}`: {err}", path.display()))?;

            let entry = per_file.entry(path).or_default();
            for edit in edits {
                let span = std::ops::Range {
                    start: cosh_sdk::lsp::position_to_offset(&text, edit.range.start, encoding),
                    end: cosh_sdk::lsp::position_to_offset(&text, edit.range.end, encoding),
                };
                entry.push(super::support::PlannedEdit {
                    span,
                    new_text: edit.new_text.clone(),
                    line: usize::try_from(edit.range.start.line).unwrap_or(0) + 1,
                });
            }
        }
    }

    for (path, planned) in &per_file {
        super::support::apply_edits(path, planned)?;
    }
    Ok(())
}

fn render_list(actions: &[CodeActionEntry]) -> String {
    if actions.is_empty() {
        return "No code actions available.".to_owned();
    }
    actions
        .iter()
        .map(|action| {
            let kind = action.kind.as_deref().unwrap_or("action");
            let files_note = if action.files.is_empty() {
                String::new()
            } else {
                format!(" [{}]", action.files.join(", "))
            };
            format!("[{}] ({kind}) {}{}", action.index, action.title, files_note)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
