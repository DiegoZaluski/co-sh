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
    /// Optional 1-based inclusive line range(s) to read exactly, e.g.
    /// `"50-100"` or `"10-20,200-220"` (comma-separated for multiple
    /// disjoint ranges). Takes precedence over `line`/`symbol` and performs a
    /// plain line slice — no AST block resolution — so the agent reads only
    /// what it asked for (port of the oh-my-pi range selector).
    pub line_range: Option<String>,
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

// ---------------------------------------------------------------------------
// AST engine (port of oh-my-pi `ast_edit`)
// ---------------------------------------------------------------------------

/// A single structural rewrite op for the AST engine.
///
/// `pat` is an AST-aware pattern (ast-grep style) that may use metavariables
/// such as `$NAME` (matches exactly one node) and `$$$NAME` (matches zero or
/// more nodes, e.g. an argument list). `out` is the replacement template;
/// metavariables referenced there are substituted with the text captured when
/// `pat` matched.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AstEditOp {
    /// AST pattern to match against the file's syntax tree.
    pub pat: String,
    /// Replacement template. Metavariables from `pat` may be referenced.
    pub out: String,
}

/// The AST engine argument (arg 2 of the edit tool).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FsAstEdit {
    /// Structural rewrite operations; applied in order over each matched file.
    pub ops: Vec<AstEditOp>,
    /// Files, directories, or globs to rewrite.
    pub paths: Vec<String>,
    /// Hard cap on the number of files edited in one call (defaults to
    /// [`crate::fs::ast_edit::DEFAULT_MAX_FILES`]).
    #[serde(default)]
    pub max_files: Option<usize>,
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
