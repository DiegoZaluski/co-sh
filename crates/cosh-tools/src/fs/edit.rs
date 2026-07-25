use cosh_sdk::hashline::{
    diff::structured_patch,
    format::HL_FILE_PREFIX,
    fs::DiskFilesystem,
    input::Patch,
    patcher::Patcher,
    types::{BlockResolver, BlockResolverRequest, BlockSpan, SplitOptions},
};
use cosh_sdk::rollback;

use super::types::{EditTarget, FsEdit, FsMetadata};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct EditResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub first_changed_line: Option<u32>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[allow(clippy::needless_pass_by_value)]
fn resolve_block_fn(req: BlockResolverRequest) -> Option<BlockSpan> {
    cosh_sdk::tree_sitter::tree_sitter().resolve_block(&req.path, &req.text, req.line)
}

/// Apply edits to one or more files.
///
/// # Errors
///
/// Returns `Err` if a file hash doesn't match, edit operations fail to parse,
/// or the underlying filesystem returns an error.
pub async fn edit(metadata: FsMetadata, tg: FsEdit) -> Result<Vec<EditResult>, String> {
    let mut results = Vec::new();

    for target in tg.targets.clone() {
        let result = edit_target(target, &metadata).await?;
        results.push(result);
    }

    Ok(results)
}

async fn edit_target(target: EditTarget, metadata: &FsMetadata) -> Result<EditResult, String> {
    let validated_path = metadata.fs_guard(&target.path)?;

    let path_str = validated_path.to_string_lossy().to_string();

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
        .map_err(|e| format!("edit failed for `{}`: {}", target.path, e))?;

    let _ = rollback::record(&path_str, &prepared.normalized);

    let section = patcher
        .commit(prepared)
        .await
        .map_err(|e| format!("edit failed for `{}`: {}", target.path, e))?;

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
        diff,
    })
}
