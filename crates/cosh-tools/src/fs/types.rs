use std::path::Path;

use crate::util::guards::{GuardResult, validate_path};

/// A single read specification.
pub struct Target<'a> {
    pub path: &'a str,
    pub line: Option<usize>,
    pub symbol: Option<&'a str>,
}

/// One or more file read operations.
///
/// ```ignore
/// ReadFile { read: vec![
///     Target { path: "src/main.rs", line: Some(5), symbol: None },
///     Target { path: "src/lib.rs", line: None, symbol: Some("run") },
/// ] }
/// ```
pub struct ReadFile<'a> {
    pub read: Vec<Target<'a>>,
}

pub struct TargetFile<'a> {
    pub text: &'a str,
    pub path: &'a str,
}

pub struct WriteAllFile<'a> {
    pub write: Vec<TargetFile<'a>>,
}

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
#[derive(Debug, Clone, Default)]
pub struct EditFile<'a> {
    pub edit: Vec<EditTarget<'a>>,
}

/// Input for a single rollback operation.
///
/// Provide `path` and optionally `hash`. If `hash` is empty the engine
/// restores the version immediately before the current file content.
///
/// The hash comes from a `¶path#HASH` header previously returned by any
/// read, write, or edit operation in this session.
///
/// ```ignore
/// RollbackInput { path: "src/main.rs", hash: "A3B2" } // restore to A3B2
/// RollbackInput { path: "src/main.rs", hash: "" }    // restore previous
/// ```
pub struct RollbackInput<'a> {
    /// Path of the file to restore.
    pub path: &'a str,
    /// Hash of the target version from a `¶path#HASH` header.
    /// Pass an empty string to restore the immediately preceding version.
    pub hash: &'a str,
}

//___
#[derive(Debug, PartialEq)]
pub(crate) enum FsGuard {
    Allowed,
    Denied,
    Mismatch(String),
}

impl FsMetadata<'_> {
    pub(crate) fn fs_guard(&self, path: &str) -> FsGuard {
        let allowlist = self.write_path_allowlist.as_deref();
        let blocklist = self.write_path_blocklist.as_deref();

        match validate_path(path, self.root, allowlist, blocklist) {
            GuardResult::Allowed(_) => FsGuard::Allowed,
            GuardResult::Denied(_) => FsGuard::Denied,
            GuardResult::Mismatch(msg) => FsGuard::Mismatch(msg),
        }
    }
}
