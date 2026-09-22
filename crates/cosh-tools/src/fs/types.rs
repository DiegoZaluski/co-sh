use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::util::path_guard::PathGuard;

/// One passive LSP finding attached to an fs operation result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LspNote {
    /// Workspace path of the file the finding refers to.
    pub path: String,
    /// 1-based line where the finding starts.
    pub line: u32,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Emitting tool (e.g. `rustc`, `tsc`), when the server reports one.
    pub source: Option<String>,
}

/// Structured LSP feedback for one fs operation, split by severity so the
/// TUI can color errors and warnings differently and the model gets precise
/// anchors. `None`/empty means the operation produced no findings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LspNotes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<LspNote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<LspNote>,
}

/// A single read specification.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Target {
    pub path: String,
    pub line: Option<usize>,
    pub symbol: Option<String>,
    /// Optional 1-based inclusive line range(s) to read exactly, e.g.
    /// `"50-100"` or `"10-20,200-220"` (comma-separated for multiple
    /// disjoint ranges). Performs a plain line slice — no AST block
    /// resolution — so the agent reads only what it asked for. Checked
    /// before `line`, but after `symbol`: if `symbol` is set it wins.
    pub line_range: Option<String>,
}

/// Configuration for file read operations.
#[derive(Default, Debug, Deserialize, JsonSchema)] // #[derive(Debug, Deserialize, JsonSchema)]
pub struct FsRead {
    pub targets: Vec<Target>,
}
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
pub struct TargetFile {
    /// Content to write. The advertised schema uses `content` (the field
    /// name every mainstream write tool trains on); `text` is the legacy
    /// batch-form name, kept as the primary field with `content` as a serde
    /// alias. NOT `#[serde(default)]`: a missing `content` must fail at
    /// parse time (surfacing the schema hint), not fall through to an
    /// empty-write warning.
    #[serde(alias = "content")]
    pub text: String,
    pub path: String,
    /// Optional file hash from a previous `read`. When the target file
    /// already exists on disk the hash is **required** — the write is
    /// rejected if it is missing or stale.  For brand-new files the
    /// field is ignored.
    #[serde(default)]
    pub file_hash: Option<String>,
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

/// Configuration for file edit operations (hashline replace engine).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FsEdit {
    /// Edit targets. Multi-file batching is intentionally NOT advertised by
    /// the tool schema: single-path calls proved far more reliable in agent
    /// sessions (batched calls raised schema-error rates). The batch form is
    /// kept — not removed — for future studies and benchmarks.
    pub targets: Vec<EditTarget>,
    /// Preview mode: apply in memory only, returning the diff and the
    /// syntax-probe verdict without writing anything. Re-issue without
    /// `dry_run` to apply for real.
    #[serde(default)]
    pub dry_run: bool,
}

/// One content-anchored replacement.
///
/// Public call shape is flat: `fs_edit` unwraps the single advertised
/// `path/old_string/new_string` object into this struct (batch form kept for
/// benchmarks).
///
/// `old_string` is a content address: it must match the file exactly (the
/// match must be unique unless [`Self::replace_all`] is set). The edit is
/// still bound to a hashline snapshot tag, so drift between read and edit
/// keeps being detected — the match runs against the tagged snapshot and the
/// result flows through the same hashline apply pipeline as `targets`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReplaceEdit {
    pub path: String,
    /// 4-hex content hash tag: the `¶path#TAG` anchor from your last read
    /// (or a previous edit result). Required for the first edit of each file
    /// in the call; follow-up edits to the same file in the same call may
    /// omit it — they chain on the fresh tag produced by the previous edit.
    #[serde(default)]
    pub file_hash: Option<String>,
    /// Exact text to replace, copied verbatim including whitespace and
    /// newlines. Must occur exactly once unless [`Self::replace_all`] is set;
    /// use the smallest snippet that is unique.
    pub old_string: String,
    /// Replacement text. Empty deletes the matched text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring a unique match.
    #[serde(default)]
    pub replace_all: bool,
}

/// Arguments for the content edit engine.
///
/// The tool schema advertises one edit (one path) per call; the `Vec` batch
/// form is kept — not removed — for future studies and benchmarks (batched
/// calls raised schema-error rates in agent sessions). Same-file edits in a
/// batch still chain: follow-ups may omit `file_hash`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FsContentEdit {
    pub edits: Vec<ReplaceEdit>,
    /// Preview mode: apply in memory only, returning the diff and the
    /// syntax-probe verdict without writing anything. Re-issue without
    /// `dry_run` to apply for real.
    #[serde(default)]
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// AST engine
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

/// The AST engine argument.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FsAstEdit {
    /// Structural rewrite operations; applied in order over each matched file.
    pub ops: Vec<AstEditOp>,
    /// Files, directories, or globs to rewrite. The advertised schema is one
    /// path per call; multi-path glob sweeps are kept for codemods and
    /// future benchmarks.
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
