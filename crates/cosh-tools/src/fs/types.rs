use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::path_guard::PathGuard;

/// A single read specification.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Target {
    pub path: String,
    pub line: Option<usize>,
    pub symbol: Option<String>,
}

/// Configuration for file read operations.
#[derive(Default, Debug, Deserialize, JsonSchema)] // #[derive(Debug, Deserialize, JsonSchema)]
pub struct FsRead {
    pub targets: Vec<Target>,
}
#[derive(Default, Debug, Deserialize, JsonSchema)]
pub struct TargetFile {
    pub text: String,
    pub path: String,
}

/// Configuration for file write operations.
#[derive(Default, Debug, Deserialize, JsonSchema)]
pub struct FsWrite {
    pub targets: Vec<TargetFile>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FsMetadata {
    pub root: PathBuf,
    pub allowlist: Option<Vec<PathBuf>>,
    pub blocklist: Option<Vec<PathBuf>>,
}

// ___
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct EditTarget {
    pub path: String,
    pub file_hash: String,
    pub ops: String,
}

/// Configuration for file edit operations.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FsEdit {
    pub targets: Vec<EditTarget>,
}

/// Parameters for file rollback operations.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FsRollbackInput {
    pub path: String,
    pub hash: String,
}

/// Configuration for file rollback operations.
#[derive(Default)]
pub struct FsRollback;

impl FsMetadata {
    /// Validate `path` against the project root, allowlist, and blocklist.
    ///
    /// Returns the canonicalized safe path on success, or a descriptive error
    /// string explaining why the path was denied.
    ///
    /// Delegates to [`PathGuard`] for all validation and canonicalization logic.
    pub(crate) fn fs_guard(&self, path: &str) -> Result<PathBuf, String> {
        let guard = PathGuard::new(
            &self.root,
            self.allowlist.as_deref(),
            self.blocklist.as_deref(),
        );
        guard.resolve(path)
    }
}
