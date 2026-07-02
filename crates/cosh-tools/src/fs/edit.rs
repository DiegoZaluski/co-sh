use cosh_sdk::hashline::{
    format::{HL_FILE_HASH_SEP, HL_FILE_PREFIX, compute_file_hash, format_hashline_header},
    fs::{DiskFilesystem, Filesystem},
    input::Patch,
    normalize,
    types::{BlockResolver, BlockResolverRequest, BlockSpan, SplitOptions},
};

use super::types::{EditTarget, FsEdit, FsMetadata};
use cosh_sdk::rollback;
#[derive(Debug)]
pub struct EditResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub first_changed_line: Option<u32>,
    pub warnings: Vec<String>,
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
    let fs = DiskFilesystem::new();
    let mut results = Vec::new();

    for target in tg.targets.clone() {
        let result = edit_target(fs.clone(), target, metadata.clone()).await?;
        results.push(result);
    }

    Ok(results)
}

use super::types;
async fn edit_target(
    fs: DiskFilesystem,
    target: EditTarget,
    metadata: FsMetadata,
) -> Result<EditResult, String> {
    let validated_path = match metadata.fs_guard(&target.path) {
        types::FsGuard::Allowed(path) => path,
        types::FsGuard::Denied => {
            return Err(format!("write permissions denied for `{}`", target.path));
        }
        types::FsGuard::Mismatch(msg) => {
            return Err(msg);
        }
    };

    let path_str = validated_path.to_string_lossy();
    let raw = fs.read_text(&path_str).await.map_err(|_| {
        format!(
            "file `{}` not found. Use the write tool to create new files.",
            target.path
        )
    })?;

    let bom_result = normalize::strip_bom(&raw);
    let normalized = normalize::normalize_to_lf(&bom_result.text);
    let _ = rollback::record(&path_str, &normalized);

    let actual_hash = compute_file_hash(&normalized);
    if actual_hash != target.file_hash {
        return Err(format!(
            "hash mismatch for `{path}`: expected `{expected}` but current file hashes to `{actual}`. \
             The file has changed since it was last read. Re-read the file and retry the edit.",
            path = target.path,
            expected = target.file_hash,
            actual = actual_hash
        ));
    }

    let hashline_input = format!(
        "{prefix}{path}{sep}{hash}\n{ops}",
        prefix = HL_FILE_PREFIX,
        sep = HL_FILE_HASH_SEP,
        path = target.path,
        hash = target.file_hash,
        ops = target.ops
    );

    let patch = Patch::parse(&hashline_input, &SplitOptions::default()).map_err(|e| {
        format!(
            "failed to parse edit operations for `{}`: {}",
            target.path, e
        )
    })?;

    let section = patch.sections.into_iter().next().ok_or_else(|| {
        format!(
            "no edit operations found for `{}`. The ops field must contain hashline operations \
             such as `replace N..M:`, `delete N..M`, `insert before|after|head|tail:`.",
            target.path
        )
    })?;

    let apply_result = section.apply_to(&normalized, Some(resolve_block_fn as BlockResolver));

    let after = apply_result.text;
    let new_hash = compute_file_hash(&after);
    let header = format_hashline_header(&path_str, &new_hash);

    let persisted = bom_result.bom + &after;

    fs.write_text(&path_str, &persisted)
        .await
        .map_err(|e| format!("failed to write `{}`: {}", target.path, e))?;

    cosh_sdk::tree_sitter::tree_sitter().invalidate(&path_str);
    let _ = rollback::record(&path_str, &after);

    Ok(EditResult {
        path: target.path.clone(),
        file_hash: new_hash,
        header,
        first_changed_line: apply_result.first_changed_line,
        warnings: apply_result.warnings,
    })
}
