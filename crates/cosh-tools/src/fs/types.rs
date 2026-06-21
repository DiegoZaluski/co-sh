use std::path::{Path, PathBuf};

use crate::util::guards::{GuardResult, normalize_path, validate_path};

/// A single read specification.
pub struct Target<'a> {
    pub path: &'a str,
    pub line: Option<usize>,
    pub symbol: Option<&'a str>,
}

/// Configuration for file read operations.
#[derive(Default)]
pub struct FsRead;

pub struct TargetFile<'a> {
    pub text: &'a str,
    pub path: &'a str,
}

/// Configuration for file write operations.
#[derive(Default)]
pub struct FsWrite;

#[derive(Debug, Clone)]
pub struct FsMetadata<'a> {
    pub root: &'a Path,
    pub write_path_allowlist: Option<Vec<&'a Path>>,
    pub write_path_blocklist: Option<Vec<&'a Path>>,
}

// ___
#[derive(Debug, Clone, Default)]
pub struct EditTarget<'a> {
    pub path: &'a str,
    pub file_hash: &'a str,
    pub ops: &'a str,
}

/// Configuration for file edit operations.
#[derive(Debug, Clone, Default)]
pub struct FsEdit;

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

impl FsMetadata<'_> {
    pub(crate) fn fs_guard(&self, path: &str) -> FsGuard {
        let allowlist = self.write_path_allowlist.as_deref();
        let blocklist = self.write_path_blocklist.as_deref();

        match validate_path(path, self.root, allowlist, blocklist) {
            GuardResult::Allowed(normalized) => {
                let Ok(root_canon) = self.root.canonicalize() else {
                    return FsGuard::Denied;
                };

                let root_norm = normalize_path(self.root, self.root);
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
