//! `lsp_rename` engine: two-phase rename with disk application.
//!
//! Phase 1 (`confirm: false`, the default) returns an **applied: false**
//! plan — every file, every edit position — and touches nothing. Phase 2
//! (`confirm: true`) applies the exact same server answer to disk and
//! re-opens affected files so servers stay in sync.
//!
//! The two-phase split keeps the permission decision where it belongs: with
//! the model, looking at real numbers ("3 files, 12 edits") instead of a
//! blind yes/no gate buried in the stack.

use std::{
    collections::BTreeMap,
    ops::Range,
    path::{Path, PathBuf},
};

use cosh_sdk::lsp::{
    PositionEncoding,
    lsp_types::{
        DocumentChangeOperation, DocumentChanges, OneOf as LspOneOf, TextEdit, WorkspaceEdit,
        request::{Rename as RenameRequest, Request as _},
    },
    position_to_offset, uri_to_path,
};
use serde_json::json;

use super::{
    support,
    types::{RenameFilePlan, RenameInput, RenameOutput},
};

/// Resolve `textDocument/rename`; phase semantics in the module docs.
pub async fn run_rename(
    deps: &super::support::Deps<'_>,
    input: &RenameInput,
) -> Result<RenameOutput, String> {
    if input.new_name.trim().is_empty() {
        return Err("`new_name` cannot be empty".into());
    }

    let path = deps.manager.root().join(&input.file_path);
    let (position, clients) =
        support::resolve_target(deps, &path, &input.position, &input.symbol).await?;

    let params = json!({
        "textDocument": { "uri": support::uri(&path)? },
        "position": position,
        "newName": input.new_name,
    });

    let response: Option<WorkspaceEdit> = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| {
            caps.rename_provider
                .as_ref()
                .is_some_and(|provider| match provider {
                    LspOneOf::Left(enabled) => *enabled,
                    LspOneOf::Right(_) => true,
                })
        },
        |_client| (RenameRequest::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let workspace_edit =
        response.ok_or_else(|| "the server cannot rename this symbol".to_owned())?;

    // Edits must be spliced with the same units they were computed in.
    let encoding = clients[0].position_encoding();
    let plans = collect_edits(&workspace_edit, encoding)?;

    if plans.is_empty() {
        return Err("the server returned an empty rename plan".into());
    }

    let total_edits: usize = plans.values().map(|edits| edits.len()).sum();
    let files: Vec<RenameFilePlan> = plans
        .iter()
        .map(|(file_path, edits)| RenameFilePlan {
            path: file_path.clone(),
            edits: edits.len(),
            preview: edits.iter().take(3).map(preview_line).collect(),
        })
        .collect();

    if !input.confirm.unwrap_or(false) {
        let formatted = render_plan(&files, total_edits, false);
        return Ok(RenameOutput {
            applied: false,
            files,
            total_edits,
            formatted,
        });
    }

    let mut applied_files = Vec::with_capacity(plans.len());
    for (file_path, planned) in &plans {
        let path = Path::new(file_path);
        support::apply_edits(path, planned)?;
        for client in &clients {
            let _ = client.touch_file(path).await;
        }
        applied_files.push(RenameFilePlan {
            path: file_path.clone(),
            edits: planned.len(),
            preview: Vec::new(),
        });
    }

    let formatted = render_plan(&applied_files, total_edits, true);
    Ok(RenameOutput {
        applied: true,
        files: applied_files,
        total_edits,
        formatted,
    })
}

/// Flatten a WorkspaceEdit into per-file planned replacements, sorted
/// back-to-front by byte offset so splicing never shifts pending ranges.
fn collect_edits(
    edit: &WorkspaceEdit,
    encoding: PositionEncoding,
) -> Result<BTreeMap<String, Vec<support::PlannedEdit>>, String> {
    let mut out: BTreeMap<String, Vec<support::PlannedEdit>> = BTreeMap::new();

    // Our capabilities announce documentChanges=false, so conforming servers
    // use `changes` — but handle both defensively; some ignore negotiation.
    if let Some(document_changes) = &edit.document_changes {
        match document_changes {
            DocumentChanges::Edits(edits) => {
                for doc_edit in edits {
                    for edit in doc_edit.edits.iter() {
                        let text_edit = match edit {
                            LspOneOf::Left(edit) => edit,
                            LspOneOf::Right(annotated) => &annotated.text_edit,
                        };
                        push_edit(
                            uri_to_path(&doc_edit.text_document.uri),
                            text_edit,
                            encoding,
                            &mut out,
                        );
                    }
                }
            }
            DocumentChanges::Operations(ops) => {
                for op in ops {
                    match op {
                        DocumentChangeOperation::Op(resource_op) => {
                            log::warn!(
                                "rename plan carries an unhandled resource op: {resource_op:?}"
                            );
                        }
                        DocumentChangeOperation::Edit(doc_edit) => {
                            for edit in doc_edit.edits.iter() {
                                let text_edit = match edit {
                                    LspOneOf::Left(edit) => edit,
                                    LspOneOf::Right(annotated) => &annotated.text_edit,
                                };
                                push_edit(
                                    uri_to_path(&doc_edit.text_document.uri),
                                    text_edit,
                                    encoding,
                                    &mut out,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            for edit in edits {
                push_edit(uri_to_path(uri), edit, encoding, &mut out);
            }
        }
    }

    Ok(out)
}

fn push_edit(
    target: Option<PathBuf>,
    edit: &TextEdit,
    encoding: PositionEncoding,
    out: &mut BTreeMap<String, Vec<support::PlannedEdit>>,
) {
    let Some(path) = target else {
        log::warn!("skipping non-file rename target");
        return;
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            log::warn!(
                "skipping unreadable rename target `{}`: {err}",
                path.display()
            );
            return;
        }
    };

    let span = Range {
        start: position_to_offset(&text, edit.range.start, encoding),
        end: position_to_offset(&text, edit.range.end, encoding),
    };

    out.entry(path.display().to_string())
        .or_default()
        .push(support::PlannedEdit {
            span,
            new_text: edit.new_text.clone(),
            line: usize::try_from(edit.range.start.line).unwrap_or(0) + 1,
        });
}

/// Splice one file's edits back-to-front. Overlaps abort before any write.
fn preview_line(edit: &support::PlannedEdit) -> String {
    format!("L{} → {}", edit.line, ellipsize(&edit.new_text, 60))
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let single = text.replace('\n', "⏎ ");
    if single.chars().count() <= max_chars {
        single
    } else {
        let cut: String = single.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

fn render_plan(files: &[RenameFilePlan], total_edits: usize, applied: bool) -> String {
    let mut lines = Vec::new();
    if applied {
        lines.push(format!(
            "Renamed across {} file(s), {total_edits} edit(s):",
            files.len()
        ));
    } else {
        lines.push(format!(
            "Rename plan — {total_edits} edit(s) across {} file(s). \
             Re-call with `confirm: true` to apply:",
            files.len()
        ));
    }
    for file in files {
        lines.push(format!("{} ({} edit(s))", file.path, file.edits));
        for preview in &file.preview {
            lines.push(format!("  {preview}"));
        }
    }
    lines.join("\n")
}
