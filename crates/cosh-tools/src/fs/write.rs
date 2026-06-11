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
use super::fs_guard::{FsGuard, fs_guard};
use super::types::{FsMetadata, WriteAllFile};

use cosh_sdk::hashline::fs::Filesystem;
use cosh_sdk::hashline::{format, fs::DiskFilesystem};

pub fn write(wtarget: WriteAllFile, metadata: FsMetadata) -> Result<String, String> {
    let mut result = vec![];
    let fs = DiskFilesystem::new();

    for target in &wtarget.write {
        if target.text.trim().is_empty() {
            result.push(format!(
                "text is empty for `{path}`. Nothing was sent to add to the file.",
                path = target.path
            ));
            continue;
        }

        match fs_guard(metadata.clone(), target.path) {
            FsGuard::Allowed => {
                if let Err(err) = fs.write_text(target.path, target.text) {
                    result.push(format!("failed to write `{}`: {}", target.path, err));
                    continue;
                }

                cosh_sdk::syntax::syntax().invalidate(target.path);

                let hash = format::compute_file_hash(target.text);
                let header = format::format_hashline_header(target.path, &hash);
                result.push(header);
            }
            FsGuard::Denied => {
                result.push(format!(
                    "write permission denied for `{}`. \
                     Files under `{:?}` are writable by default. \
                     Use the allowlist to grant access to paths outside this directory.",
                    target.path, metadata.root
                ));
            }
            FsGuard::Mismatch(message) => {
                return Err(message);
            }
        }
    }
    Ok(result.join("\n"))
}
