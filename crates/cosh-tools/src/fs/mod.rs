//! Shared-state wrapper for file-system tool operations.
//!
//! [`Fs`] holds the project root and write-scope guards so callers don't
//! have to construct [`FsMetadata`] on every invocation.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::fs::Fs;
//!
//! let fs = Fs::new().cwd("/home/user/project");
//! fs.write(vec![TargetFile { path: "foo.txt", text: "hello" }]).await;
//! ```

pub mod edit;
pub mod read;
pub mod rollback;
#[cfg(test)]
mod test;
pub mod types;
pub mod write;

use std::path::PathBuf;

pub use edit::{EditResult, edit};
pub use read::{ReadResult, read};
pub use rollback::{RollbackResult, rollback};
pub use types::{EditTarget, FsEdit, FsMetadata, FsRead, FsRollback, FsWrite, Target, TargetFile};
pub use write::{WriteResult, write};

/// Shared-state wrapper for file-system tool operations.
///
/// Use the builder methods after [`new`](Self::new) to configure the
/// project root and write-scope guards, then call the operation methods
/// directly.
pub struct Fs {
    root: PathBuf,

    /// Restricts write operations to the specified paths.
    /// Your frontend should request confirmation before setting this field.
    allowlist: Option<Vec<PathBuf>>,
    blocklist: Option<Vec<PathBuf>>,
}

impl Default for Fs {
    fn default() -> Self {
        Self::new()
    }
}

impl Fs {
    /// Create a new `Fs` with no root path set.
    ///
    /// All paths are denied until [`cwd`](Self::cwd) is called.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: PathBuf::new(),
            allowlist: None,
            blocklist: None,
        }
    }

    /// Set the project root directory (used for path-validation guards).
    #[must_use]
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.root = path.into();
        self
    }

    /// Set the explicit write-path allowlist.
    #[must_use]
    pub fn allowlist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.allowlist = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Set the explicit write-path blocklist.
    #[must_use]
    pub fn blocklist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.blocklist = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Build an [`FsMetadata`] from the current owned state.
    fn metadata(&self) -> FsMetadata<'_> {
        let allowlist = self
            .allowlist
            .as_ref()
            .map(|v| v.iter().map(PathBuf::as_path).collect());

        let blocklist = self
            .blocklist
            .as_ref()
            .map(|v| v.iter().map(PathBuf::as_path).collect());

        FsMetadata {
            root: &self.root,
            allowlist,
            blocklist,
        }
    }

    /// Read one or more files / symbols.
    ///
    /// See [`read`] for details.
    pub async fn read(&self, targets: Vec<Target<'_>>) -> Vec<ReadResult> {
        read(&FsRead, self.metadata(), targets).await
    }

    /// Write content to one or more files.
    ///
    /// See [`write`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the allowlist/blocklist configuration is invalid.
    pub async fn write(&self, targets: Vec<TargetFile<'_>>) -> Result<Vec<WriteResult>, String> {
        write(&FsWrite, self.metadata(), targets).await
    }

    /// Apply edits to one or more files.
    ///
    /// See [`edit`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if a file hash doesn't match, edit operations fail
    /// to parse, or write permissions are denied.
    pub async fn edit(&self, targets: Vec<EditTarget<'_>>) -> Result<Vec<EditResult>, String> {
        edit(&FsEdit, self.metadata(), targets).await
    }

    /// Roll back a file to a previously recorded session version.
    ///
    /// See [`rollback`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if write permission is denied or the path has no
    /// rollback history.
    pub async fn rollback(&self, path: &str, hash: &str) -> Result<RollbackResult, String> {
        rollback(&FsRollback, self.metadata(), path, hash).await
    }
}
