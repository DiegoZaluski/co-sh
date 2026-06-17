//! Overwrites one or more files completely.
//!
//! Verifies write-scope permissions using an [`FsMetadata`] structure that describes:
//! - The project root directory.
//! - Paths explicitly granted write permission.
//! - Paths explicitly denied write permission.
//!
//! If no paths are explicitly blocked, any file within the root directory
//! is considered writable by default.
//!
//! # Errors
//!
//! Returns `Err` if the [`FsMetadata`] has inconsistent allowlist/blocklist entries.
//! Individual file write failures are reported inline in the returned string
//! rather than aborting the batch.
use super::types::{FsMetadata, WriteAllFile};

use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore},
};

#[derive(Debug)]
pub struct WriteResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub warnings: Option<String>,
}

use super::types;
/// Write content to one or more files.
///
/// # Errors
///
/// Returns `Err` if the [`FsMetadata`] has inconsistent allowlist/blocklist entries,
/// or a path is both blocked and allowed simultaneously.
pub async fn write(
    wtarget: WriteAllFile<'_>,
    metadata: FsMetadata<'_>,
) -> Result<Vec<WriteResult>, String> {
    let mut result: Vec<WriteResult> = vec![];
    let fs = DiskFilesystem::new();
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    for target in &wtarget.write {
        if target.text.trim().is_empty() {
            let warning = format!(
                "text is empty for `{path}`. Nothing was sent to add to the file.",
                path = target.path
            );
            let res = WriteResult {
                file_hash: String::new(),
                header: String::new(),
                path: target.path.to_string(),
                warnings: Some(warning),
            };
            result.push(res);
            continue;
        }

        match metadata.fs_guard(target.path) {
            types::FsGuard::Allowed => {
                if let Err(err) = fs.write_text(target.path, target.text).await {
                    let warning = format!("failed to write `{}`: {}", target.path, err);
                    let res = WriteResult {
                        file_hash: String::new(),
                        header: String::new(),
                        path: target.path.to_string(),
                        warnings: Some(warning),
                    };
                    result.push(res);
                    continue;
                }

                cosh_sdk::tree_sitter::tree_sitter().invalidate(target.path);

                // Retained as a backup for a potential rollback.
                store.record(target.path, target.text);

                let hash = format::compute_file_hash(target.text);
                let header = format::format_hashline_header(target.path, &hash);

                let res = WriteResult {
                    file_hash: hash,
                    header: header.clone(),
                    path: target.path.to_string(),
                    warnings: None,
                };

                result.push(res);
            }
            types::FsGuard::Denied => {
                let warning = format!(
                    "write permission denied for `{}`. \
                     Files under `{:?}` are writable by default. \
                     Use the allowlist to grant access to paths outside this directory.",
                    target.path, metadata.root.display()
                );

                let res = WriteResult {
                    file_hash: String::new(),
                    header: String::new(),
                    path: target.path.to_string(),
                    warnings: Some(warning),
                };
                result.push(res);
            }
            types::FsGuard::Mismatch(message) => {
                return Err(message);
            }
        }
    }
    Ok(result)
}
