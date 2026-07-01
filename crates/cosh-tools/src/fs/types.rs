use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::guards::{GuardResult, normalize_path, validate_path};

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
#[derive(Debug, Clone, Default)]
pub struct EditTarget {
    pub path: String,
    pub file_hash: String,
    pub ops: String,
}

/// Configuration for file edit operations.
#[derive(Debug, Clone, Default)]
pub struct FsEdit {
    pub targets: Vec<EditTarget>,
}

/// Configuration for file rollback operations.
#[derive(Default)]
pub struct FsRollback;

//___
#[derive(Debug, PartialEq)]
pub(crate) enum FsGuard {
    Allowed(PathBuf),
    Denied,
    Mismatch(String),
}

impl FsMetadata {
    pub(crate) fn fs_guard(&self, path: &str) -> FsGuard {
        let allowlist = self.allowlist.as_deref();
        let blocklist = self.blocklist.as_deref();

        match validate_path(path, &self.root, allowlist, blocklist) {
            GuardResult::Allowed(normalized) => {
                let Ok(root_canon) = self.root.canonicalize() else {
                    return FsGuard::Denied;
                };

                let root_norm = normalize_path(&self.root, &self.root);
                let in_root = normalized.starts_with(&root_norm);

                let resolved = match normalized.canonicalize() {
                    Ok(canon) => canon,
                    Err(_) => match normalized.parent() {
                        Some(parent) => match parent.canonicalize() {
                            Ok(parent_canon) => {
                                let file_name = normalized.file_name().unwrap_or_default();
                                parent_canon.join(file_name)
                            }
                            Err(_) => {
                                if in_root {
                                    normalized
                                } else {
                                    return FsGuard::Denied;
                                }
                            }
                        },
                        None => return FsGuard::Denied,
                    },
                };

                if in_root && !resolved.starts_with(&root_canon) {
                    return FsGuard::Denied;
                }

                FsGuard::Allowed(resolved)
            }
            GuardResult::Denied(_) => FsGuard::Denied,
            GuardResult::Mismatch(msg) => FsGuard::Mismatch(msg),
        }
    }
}
