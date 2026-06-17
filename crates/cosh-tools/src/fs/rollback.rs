//! Rollback tool — restores a file to a previously recorded session version.
//!
//! Wraps [`cosh_sdk::rollback::restore`] with write-permission enforcement
//! via [`FsMetadata`].

use cosh_sdk::rollback::{RestoreInput, restore};

use super::types::{FsMetadata, RollbackInput};

/// Result of a successful rollback.
#[derive(Debug)]
pub struct RollbackResult {
    /// Path of the restored file.
    pub path: String,
    /// Content hash of the version now on disk. Use this as the anchor for
    /// any follow-up read or edit operations.
    pub file_hash: String,
    /// Hashline header `¶path#HASH` for the restored version. Include this
    /// in the session context so subsequent tools have a valid anchor.
    pub header: String,
    /// Content hash of what was on disk before the restore. Pass this back
    /// to rollback to undo the restore.
    pub replaced_hash: String,
    /// Non-fatal warning when an anomaly was detected but the restore still
    /// succeeded (e.g., the file was modified externally since the snapshot).
    pub warning: Option<String>,
}

/// Restore a file to a previously recorded version.
///
/// Checks write permissions before delegating to the rollback engine. If
/// `input.hash` is empty, the engine restores the version immediately
/// preceding the current file content.
///
/// # Errors
///
/// Returns an error string when:
/// - Write permission is denied for the path.
/// - The path has no rollback history in the current session.
/// - The requested hash is not found in the session history.
/// - The file is already at the requested version.
/// - There is no preceding version to restore to.
/// - The file was modified externally and `hash` is empty.
/// - A disk write error occurs.
pub async fn rollback(
    input: RollbackInput<'_>,
    metadata: FsMetadata<'_>,
) -> Result<RollbackResult, String> {
    use super::types;

    match metadata.fs_guard(input.path) {
        types::FsGuard::Allowed => {}
        types::FsGuard::Denied => {
            return Err(format!(
                "restore permission denied for `{}`; \
                 the path is outside the project root or in the blocklist",
                input.path
            ));
        }
        types::FsGuard::Mismatch(msg) => return Err(msg),
    }

    let hash = input.hash.trim();
    let hash = if hash.is_empty() {
        None
    } else {
        Some(hash.to_owned())
    };

    let out = restore(RestoreInput {
        path: input.path.to_owned(),
        hash,
    })
    .await?;

    Ok(RollbackResult {
        path: out.path,
        file_hash: out.file_hash,
        header: out.header,
        replaced_hash: out.replaced_hash,
        warning: out.warning,
    })
}
