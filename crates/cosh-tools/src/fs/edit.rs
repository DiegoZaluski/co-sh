use cosh_sdk::hashline::{
    diff::structured_patch,
    format::{HL_FILE_PREFIX, compute_file_hash, format_hashline_header},
    fs::DiskFilesystem,
    input::Patch,
    patcher::{Patcher, merge_warnings},
    types::{BlockResolver, BlockResolverRequest, BlockSpan, SplitOptions},
};
use cosh_sdk::rollback;

use super::types::{EditTarget, FsEdit, FsMetadata};
use crate::util::path_guard::assert_editable_file;
use serde::Serialize;
use std::fmt;
use std::path::Path;

/// Marker appended to a dry-run result's warnings so the model cannot
/// mistake a preview for an applied edit.
const DRY_RUN_WARNING: &str = "Dry run: nothing was written — reissue without \
     `dry_run` to apply this exact edit.";

#[derive(Debug, Serialize)]
pub struct EditResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub first_changed_line: Option<u32>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Passive LSP feedback collected after the edit (errors by default,
    /// warnings when the caller opted in). `None` when LSP is disabled or
    /// nothing was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_notes: Option<super::types::LspNotes>,
    /// `Some(true)` when the result came from a preview (`dry_run`) — the
    /// edit was validated in memory but NOT written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

/// Failure of a multi-target [`edit`] batch.
///
/// Targets are applied in order, and order carries intention: when target N
/// fails, the batch stops there. Targets applied before N are already
/// committed and stay (see [`applied`](Self::applied)); targets after N are
/// deliberately NOT attempted ([`skipped`](Self::skipped)) because their
/// edits may depend on N succeeding first.
///
/// Only the failing target is reported as the cause — the targets that follow
/// are reported as skipped *as a consequence*, never as independent failures.
#[derive(Debug)]
pub struct EditBatchError {
    /// Path of the target that failed — the root cause of the batch abort.
    pub failed_path: String,
    /// The underlying error reported for the failing target.
    pub cause: String,
    /// Results of targets applied before the failure — already committed to disk.
    pub applied: Vec<EditResult>,
    /// Paths of targets after the failure that were never attempted.
    pub skipped: Vec<String>,
}

impl fmt::Display for EditBatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edit failed for `{}`: {}", self.failed_path, self.cause)?;
        if !self.applied.is_empty() {
            // Each applied result carries its new hashline anchor (`¶path#TAG`),
            // so the model can keep editing those files without re-reading.
            write!(
                f,
                "\nApplied before the failure (fresh tags for follow-up edits):"
            )?;
            for r in &self.applied {
                let anchor = if r.header.is_empty() {
                    r.path.clone()
                } else {
                    r.header.clone()
                };
                write!(f, "\n  {anchor}")?;
            }
        }
        if !self.skipped.is_empty() {
            let paths: Vec<&str> = self.skipped.iter().map(|s| s.as_str()).collect();
            write!(
                f,
                "\nNot applied (skipped because `{}` failed): {}",
                self.failed_path,
                paths.join(", ")
            )?;
        }
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn resolve_block_fn(req: BlockResolverRequest) -> Option<BlockSpan> {
    cosh_sdk::tree_sitter::tree_sitter().resolve_block(&req.path, &req.text, req.line)
}

/// Apply edits to one or more files, in order.
///
/// Targets are applied sequentially. If target N fails, the batch stops
/// there: targets applied before N are kept (see
/// [`EditBatchError::applied`]), and targets after N are deliberately not
/// attempted ([`EditBatchError::skipped`]) because their edits may depend on
/// N. Only the failing target is reported as the cause of the abort.
///
/// # Errors
///
/// Returns [`EditBatchError`] when a file hash doesn't match, edit operations
/// fail to parse, or the underlying filesystem returns an error.
pub async fn edit(metadata: FsMetadata, tg: FsEdit) -> Result<Vec<EditResult>, EditBatchError> {
    let mut results = Vec::new();

    for (i, target) in tg.targets.iter().enumerate() {
        match edit_target(target.clone(), &metadata, None, tg.dry_run).await {
            Ok(result) => results.push(result),
            Err(cause) => {
                let skipped = tg.targets[i + 1..].iter().map(|t| t.path.clone()).collect();
                return Err(EditBatchError {
                    failed_path: target.path.clone(),
                    cause,
                    applied: results,
                    skipped,
                });
            }
        }
    }

    Ok(results)
}

pub(crate) async fn edit_target(
    target: EditTarget,
    metadata: &FsMetadata,
    expected_after: Option<&str>,
    dry_run: bool,
) -> Result<EditResult, String> {
    let validated_path = metadata.fs_guard(&target.path)?;

    let path_str = validated_path.to_string_lossy().to_string();

    // Never modify a file that declares itself machine-generated (same guard
    // as `write`); the change would be lost on the next generation run.
    assert_editable_file(Path::new(&path_str))?;

    let hashline_input = format!(
        "{prefix}{path}#{hash}\n{ops}",
        prefix = HL_FILE_PREFIX,
        path = target.path,
        hash = target.file_hash,
        ops = target.ops,
    );

    let patch = Patch::parse(&hashline_input, &SplitOptions::default()).map_err(|e| {
        format!(
            "failed to parse edit operations for `{}`: {}",
            target.path, e
        )
    })?;

    let session_store = rollback::session_store().clone();
    let mut patcher = Patcher::new_shared(
        DiskFilesystem::new(),
        session_store,
        Some(resolve_block_fn as BlockResolver),
    );

    let prepared = patcher
        .prepare(&patch.sections[0])
        .await
        .map_err(|e| e.to_string())?;

    // Content-anchored callers pass the exact post-edit text they expect.
    // The hashline engine's boundary-echo and indent-repair heuristics may
    // legitimately rewrite a hand-authored `ops` payload, but for an exact
    // replacement they would silently corrupt the contract — reject instead,
    // before anything is written (prepare is in-memory only).
    if let Some(expected) = expected_after
        && prepared.apply_result.text != expected
    {
        return Err(format!(
            "content replace for `{}` deviated from the exact replacement: the \
             hashline engine's boundary/indent repair altered the payload. \
             Re-read the touched region and reissue with the `targets` engine \
             if the repaired form is acceptable.",
            target.path
        ));
    }

    // Dry run: stop after the in-memory apply. The model gets the diff and
    // the syntax-probe verdict (the applier pushes `edit_broke_parse_warning`
    // into the warnings when the result no longer parses) without anything
    // touching the disk — no commit, no rollback record, no LSP pull. The
    // warnings merge mirrors `commit` (parse warnings + apply warnings) so
    // the preview reports exactly what the real edit would.
    if dry_run {
        let after = &prepared.apply_result.text;
        let mut warnings = merge_warnings(&[
            Some(&prepared.parse_warnings),
            Some(&prepared.apply_result.warnings),
        ]);
        warnings.push(DRY_RUN_WARNING.to_string());
        let hash = compute_file_hash(after);
        return Ok(EditResult {
            path: target.path.clone(),
            file_hash: hash.clone(),
            header: format_hashline_header(&target.path, &hash),
            first_changed_line: prepared.apply_result.first_changed_line,
            warnings,
            lsp_notes: None,
            dry_run: Some(true),
            diff: if *after == prepared.normalized {
                None
            } else {
                Some(
                    structured_patch(&prepared.normalized, after, 3)
                        .to_unified_diff(&target.path, &target.path),
                )
            },
        });
    }

    let _ = rollback::record(&path_str, &prepared.normalized);

    let section = patcher.commit(prepared).await.map_err(|e| e.to_string())?;

    cosh_sdk::tree_sitter::tree_sitter().invalidate(&path_str);

    let diff = if section.before == section.after {
        None
    } else {
        Some(
            structured_patch(&section.before, &section.after, 3)
                .to_unified_diff(&target.path, &target.path),
        )
    };

    Ok(EditResult {
        path: target.path.clone(),
        file_hash: section.file_hash,
        header: section.header,
        first_changed_line: section.first_changed_line,
        warnings: section.warnings,
        lsp_notes: None,
        dry_run: None,
        diff,
    })
}
