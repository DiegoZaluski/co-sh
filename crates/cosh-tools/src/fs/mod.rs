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
//! fs.write(vec![TargetFile { path: "foo.txt".to_string(), text: "hello".to_string()  file_hash: None, }]).await;
//! ```

pub mod ast_edit;
pub mod edit;
pub mod fuzzy;
#[cfg(test)]
mod fuzzy_equivalence;
pub mod read;
pub mod replace;
pub mod rollback;
#[cfg(test)]
mod test;
pub mod types;
pub mod write;

use std::{path::PathBuf, sync::Arc, time::Duration};

pub use edit::{EditBatchError, EditResult, edit};
pub use read::{ReadResult, read};
pub use rollback::{RollbackResult, rollback};
pub use types::{
    AstEditOp, EditTarget, FsAstEdit, FsContentEdit, FsEdit, FsMetadata, FsRead, FsRollback,
    FsRollbackInput, FsWrite, LspNote, LspNotes, ReplaceEdit, Target, TargetFile,
};
pub use write::{WriteResult, write};

use crate::{ToolDescription, lsp::Lsp};
use cosh_sdk::lsp::lsp_types::DiagnosticSeverity;

/// Quiet-period budget for passive LSP feedback after a mutation.
const LSP_SETTLE: Duration = Duration::from_secs(2);
/// Per-server deadline for the post-mutation diagnostics pull, covering
/// `ServerCancelled` retries while the server is still analyzing.
const LSP_REACTION: Duration = Duration::from_secs(15);
/// Per-request timeout of a single diagnostics pull.
const LSP_PULL_TIMEOUT_SECS: u64 = 10;
/// Hard cap per severity in passive LSP feedback.
const LSP_MAX_NOTES: usize = 20;

/// Shared-state wrapper for file-system tool operations.
///
/// Use the builder methods after [`new`](Self::new) to configure the
/// project root and scope guards, then call the operation methods
/// directly.
pub struct Fs {
    root: PathBuf,

    /// Restricts write operations to the specified paths.
    /// Your frontend should request confirmation before setting this field.
    allowlist: Option<Vec<PathBuf>>,
    blocklist: Option<Vec<PathBuf>>,

    /// Read-only path allowlist (optional, falls back to `allowlist`).
    /// Paths listed here are readable but not writable.
    /// Set this to grant read access outside the project root without
    /// granting write access to the same paths.
    read_allowlist: Option<Vec<PathBuf>>,
    /// Read-only path blocklist (optional, falls back to `blocklist`).
    read_blocklist: Option<Vec<PathBuf>>,

    /// Language-server engine for passive diagnostics. Its presence is the
    /// toggle: set once via [`Fs::with_lsp`], every mutating operation then
    /// reports findings on the touched files.
    lsp: Option<Arc<Lsp>>,

    /// MCP Tool description for `read`.
    pub description_read: ToolDescription,
    /// MCP Tool description for `write`.
    pub description_write: ToolDescription,
    /// MCP Tool description for `edit` (content replace).
    pub description_edit: ToolDescription,
    /// MCP Tool description for `fs_edit_lines` (hashline replace).
    pub description_edit_lines: ToolDescription,
    /// MCP Tool description for `fs_ast_edit` (AST structural).
    pub description_ast_edit: ToolDescription,
    /// MCP Tool description for `rollback`.
    pub description_rollback: ToolDescription,
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
            read_allowlist: None,
            read_blocklist: None,
            lsp: None,
            description_read: serde_json::json!({
                "name": "fs_read",
                "description": concat!(
                    "Read one file, named symbols, or a range of lines from ",
                    "the project. The call can specify: a path with an optional ",
                    "`offset` (1-based line number to start reading from — reads ",
                    "the syntactic block containing that line when used without ",
                    "`limit`), an optional `limit` (number of lines to read; only ",
                    "provide together with `offset`, if the file is too large to ",
                    "read at once), or an optional `symbol` name (function, class, ",
                    "variable) to look up. `symbol` takes precedence over ",
                    "`offset`/`limit`. Every result carries a \u{00b6}path#TAG ",
                    "header; lines are numbered `N| text` so edits can anchor ",
                    "directly. After an fs_edit the response also carries the ",
                    "updated header — you only need to re-read when you want ",
                    "to SEE new content, not to edit again. Blocks larger ",
                    "than 24 lines are elided to their head/tail with a footer ",
                    "columns are truncated with `...` and flagged. After a range ",
                    "read a footer reports how many lines remain and how to ",
                    "continue — read only what you need instead of whole files."
                ),
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to read, relative to the project root"
                        },
                        "offset": {
                            "type": "integer",
                            "description": concat!(
                                "Optional 1-based line number to start reading from. Only ",
                                "provide if the file is too large to read at once. Without ",
                                "`limit`, reads the syntactic block containing that line; ",
                                "with `limit`, reads exactly offset..offset+limit-1"
                            )
                        },
                        "limit": {
                            "type": "integer",
                            "description": concat!(
                                "Optional number of lines to read. Only provide together ",
                                "with `offset`, if the file is too large to read at once"
                            )
                        },
                        "symbol": {
                            "type": "string",
                            "description": concat!(
                                "Optional symbol name to look up ",
                                "(function, class, variable) within the file"
                            )
                        }
                    },
                    "required": ["path"]
                }
            }),
            description_write: serde_json::json!({
                "name": "fs_write",
                "description": concat!(
                    "Write content to one file. Creates new files or overwrites ",
                    "existing ones entirely.\n\n",
                    "IMPORTANT: When overwriting an existing file, you MUST include the ",
                    "`file_hash` from a previous `fs_read` call. This proves you have ",
                    "read the file before overwriting it. If you omit `file_hash` on an ",
                    "existing file, the write will be rejected. For new files (that do ",
                    "not yet exist), `file_hash` is not needed."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to write, relative to the project root"
                        },
                        "content": {
                            "type": "string",
                            "description": "Full text content to write to the file"
                        },
                        "file_hash": {
                            "type": ["string", "null"],
                            "description": concat!(
                                "4-hex content hash tag from `fs_read` OR a previous ",
                                "fs_edit result (the \u{00B6}path#TAG header). REQUIRED when ",
                                "overwriting an existing file to prove you have read its ",
                                "current content. Omit or set to null for new files."
                            )
                        }
                    },
                    "required": ["path", "content"]
                }
            }),
            description_edit: Self::description_edit(),
            description_edit_lines: Self::description_edit_lines(),
            description_ast_edit: Self::description_edit_ast(),
            description_rollback: serde_json::json!({
                "name": "fs_rollback",
                "description": concat!(
                    "Roll back a file to a previously recorded session version: ",
                    "restore the state identified by the given `hash` (a 4-hex tag ",
                    "from an earlier fs_read/fs_edit/rollback result for that path). ",
                    "Pass an empty `hash` to restore the version immediately ",
                    "preceding the current content. Returns an error if the path ",
                    "has no rollback history, if the `hash` is not found in it, or ",
                    "if the file was modified externally since the snapshot."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to roll back, relative to the project root"
                        },
                        "hash": {
                            "type": "string",
                            "description": "Session hash identifying which version to restore"
                        }
                    },
                    "required": ["path", "hash"]
                }
            }),
        }
    }

    /// Attach the language-server engine. Its presence is the toggle: every
    /// mutating operation (write/edit/rollback) then reports passive LSP
    /// diagnostics on the touched files, and reads warm the servers up.
    #[must_use]
    pub fn with_lsp(mut self, lsp: Arc<Lsp>) -> Self {
        self.lsp = Some(lsp);
        self
    }

    /// Suppress passive LSP feedback for the chained call only — the `Fs`
    /// keeps its default behavior for later calls.
    #[must_use]
    pub fn without_lsp(&self) -> FsCall<'_> {
        FsCall {
            fs: self,
            lsp: None,
            include_warnings: false,
        }
    }

    /// Include warnings (not just errors) in the chained call's passive LSP
    /// feedback only — the `Fs` keeps its default behavior for later calls.
    #[must_use]
    pub fn warnings(&self) -> FsCall<'_> {
        FsCall {
            fs: self,
            lsp: self.lsp.as_ref(),
            include_warnings: true,
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

    /// Set the read-only path allowlist (falls back to [`allowlist`](Self::allowlist) when `None`).
    #[must_use]
    pub fn read_allowlist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.read_allowlist = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Set the read-only path blocklist (falls back to [`blocklist`](Self::blocklist) when `None`).
    #[must_use]
    pub fn read_blocklist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.read_blocklist = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Build a write-scope [`FsMetadata`] from the current owned state.
    fn metadata(&self) -> FsMetadata {
        FsMetadata {
            root: self.root.clone(),
            allowlist: self.allowlist.clone(),
            blocklist: self.blocklist.clone(),
        }
    }

    /// Build a read-scope [`FsMetadata`] from the current owned state.
    ///
    /// Uses `read_allowlist`/`read_blocklist` when set, falling back to
    /// the write-scope guards.
    fn read_metadata(&self) -> FsMetadata {
        FsMetadata {
            root: self.root.clone(),
            allowlist: self
                .read_allowlist
                .clone()
                .or_else(|| self.allowlist.clone()),
            blocklist: self
                .read_blocklist
                .clone()
                .or_else(|| self.blocklist.clone()),
        }
    }

    /// Read one or more files / symbols.
    ///
    /// See [`read`] for details. With [`Fs::with_lsp`] configured, each read
    /// warms the file's language servers up (no diagnostics attached).
    pub async fn read(&self, targets: Vec<Target>) -> Vec<ReadResult> {
        self.read_op(self.lsp.as_ref(), targets).await
    }

    async fn read_op(&self, lsp: Option<&Arc<Lsp>>, targets: Vec<Target>) -> Vec<ReadResult> {
        if let Some(lsp) = lsp {
            for t in &targets {
                Self::warm_lsp(lsp, &self.root, &t.path).await;
            }
        }
        read(self.read_metadata(), FsRead { targets }).await
    }

    /// Write content to one or more files.
    ///
    /// See [`write`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the allowlist/blocklist configuration is invalid.
    pub async fn write(&self, targets: Vec<TargetFile>) -> Result<Vec<WriteResult>, String> {
        self.write_op(self.lsp.as_ref(), false, targets).await
    }

    async fn write_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        targets: Vec<TargetFile>,
    ) -> Result<Vec<WriteResult>, String> {
        let mut results = write(self.metadata(), FsWrite { targets }).await?;
        if let Some(lsp) = lsp {
            for result in &mut results {
                result.lsp_notes =
                    Self::passive_notes(lsp, &self.root, &result.path, include_warnings).await;
            }
        }
        Ok(results)
    }

    /// Apply content-anchored replacements (the `fs_edit` wrapper over the
    /// content replace engine). Each edit replaces an exact,
    /// uniquely-matching `old_string` with `new_string`, anchored on the
    /// file's content-hash snapshot tag.
    ///
    /// # Errors
    ///
    /// Returns an error when a tag is missing/unknown, `old_string` does not
    /// match exactly or is ambiguous, or the underlying edit fails.
    pub async fn edit(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.edit_op(self.lsp.as_ref(), false, args).await
    }

    /// Edit lines from the `fs_edit_lines` wrapper over the hashline replace
    /// engine (the legacy `targets`/`edit` core entry point).
    ///
    /// Accepts the advertised flat `{path, file_hash, ops}` object or the
    /// legacy `{targets: [...]}` batch form (kept for benchmarks; not
    /// advertised by the schema).
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments do not match either accepted
    /// shape, or when the engine fails.
    pub async fn edit_lines(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.edit_lines_op(self.lsp.as_ref(), false, args).await
    }

    /// Structural syntax-tree rewrite from the `fs_edit_ast` wrapper over the
    /// AST engine. Accepts the advertised flat `{path, ops: [{pat, out}]}`
    /// object or the legacy `{ast: {...}}` / `{paths: [...]}` batch forms
    /// (kept for codemods/benchmarks; not advertised by the schema).
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments do not match either accepted
    /// shape, or when the engine fails.
    pub async fn edit_ast(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.edit_ast_op(self.lsp.as_ref(), false, args).await
    }

    async fn edit_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        args: serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let metadata = self.metadata();
        let mut results = self.edit_content(&metadata, &args).await?;
        Self::attach_edit_notes(self, lsp, include_warnings, &mut results).await;
        Ok(results)
    }

    async fn edit_lines_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        args: serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let metadata = self.metadata();
        let mut results = self.edit_replace(&metadata, &args).await?;
        Self::attach_edit_notes(self, lsp, include_warnings, &mut results).await;
        Ok(results)
    }

    async fn edit_ast_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        args: serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let metadata = self.metadata();
        let mut results = self.edit_ast_engine(&metadata, &args).await?;
        Self::attach_edit_notes(self, lsp, include_warnings, &mut results).await;
        Ok(results)
    }

    /// Shared post-edit passive LSP feedback. Previews never wrote anything:
    /// the diagnostics would describe the UNCHANGED on-disk file, not the edit.
    async fn attach_edit_notes(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        results: &mut [EditResult],
    ) {
        let Some(lsp) = lsp else { return };
        for result in results {
            if result.dry_run == Some(true) {
                continue;
            }
            result.lsp_notes =
                Self::passive_notes(lsp, &self.root, &result.path, include_warnings).await;
        }
    }

    /// Apply content-anchored replacements (the typed form of the `edits`
    /// argument of the legacy batch API). Each edit replaces an exact,
    /// uniquely-matching `old_string` with `new_string`, anchored on the
    /// file's content-hash snapshot tag.
    ///
    /// # Errors
    ///
    /// Returns an error when a tag is missing/unknown, `old_string` does not
    /// match exactly or is ambiguous, or the underlying edit fails.
    pub async fn replace_edits(&self, edits: Vec<ReplaceEdit>) -> Result<Vec<EditResult>, String> {
        self.replace_edits_op(self.lsp.as_ref(), false, edits).await
    }

    async fn replace_edits_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        edits: Vec<ReplaceEdit>,
    ) -> Result<Vec<EditResult>, String> {
        let metadata = self.metadata();
        let mut results = replace::content_edit(&metadata, &edits, false)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(lsp) = lsp {
            for result in &mut results {
                result.lsp_notes =
                    Self::passive_notes(lsp, &self.root, &result.path, include_warnings).await;
            }
        }
        Ok(results)
    }

    /// MCP `fs_edit_lines` description (the hashline replace engine).
    ///
    /// Dedicated tool for line/block edits: the model never has to pick an
    /// engine by argument shape — the tool name selects it.
    fn description_edit_lines() -> ToolDescription {
        serde_json::json!({
            "name": "fs_edit_lines",
            "description": concat!(
                "Apply targeted line/block edits to ONE file per call. Edits are ",
                "anchored by the file's content hash for safety. ",
                "Supports replace, delete, insert (before/after/head/tail), and ",
                "syntactic block operations. A successful edit returns the ",
                "updated \u{00B6}path#TAG header — use it directly for follow-up ",
                "edits on the same file without re-reading. ",
                "Example: {\"path\": \"src/main.rs\", \"file_hash\": \"3C4D\", ",
                "\"ops\": \"replace 5..7:\\n+fn hello() {\\n+    println!(\\\"hi\\\");\\n+}\"}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to the project root"
                    },
                    "file_hash": {
                        "type": "string",
                        "description": concat!(
                            "4-hex content hash tag: the \u{00B6}path#TAG anchor. ",
                            "Get it from your last fs_read OR from a previous ",
                            "fs_edit result (the `header` field carries the ",
                            "updated tag). Copy verbatim."
                        )
                    },
                    "ops": {
                        "type": "string",
                        "description": concat!(
                            "Hashline edit operations. Each operation is on its own line:\n",
                            "- replace N..M:  replace lines N through M with new content\n",
                            "   (prefix each replacement line with +)\n",
                            "- delete N..M   delete lines N through M\n",
                            "- insert before N:  insert lines before line N\n",
                            "- insert after N:   insert lines after line N\n",
                            "- insert head:      insert at start of file\n",
                            "- insert tail:      insert at end of file\n",
                            "- replace block N:  replace syntactic block at line N\n",
                            "Example:\n",
                            "  replace 5..7:\n",
                            "  +fn hello() {\n",
                            "  +    println!(\"hi\");\n",
                            "  +}\n",
                            "  delete 10..12\n",
                            "  insert after 15:\n",
                            "  +// new comment"
                        )
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": concat!(
                            "Preview mode: the edit is applied in memory only and the ",
                            "result carries the unified diff plus the syntax-probe ",
                            "verdict without writing anything. Re-issue without dry_run ",
                            "to apply the exact same edit."
                        )
                    }
                },
                "required": ["path", "file_hash", "ops"]
            }
        })
    }

    /// MCP `fs_ast_edit` description (the AST structural engine).
    fn description_edit_ast() -> ToolDescription {
        serde_json::json!({
            "name": "fs_ast_edit",
            "description": concat!(
                "Apply structural syntax-tree edits to ONE file per call. ",
                "Rewrite each match of a pattern (`pat`) to a template (`out`). ",
                "Patterns must be full, structurally valid source snippets; they support ",
                "metavariables $NAME (matches exactly one node) and $$$NAME (matches zero or ",
                "more nodes, e.g. an argument list), and the same metavariable must capture ",
                "identical text in every occurrence to match. A string literal pattern must ",
                "include its quotes, and a metavariable captures a whole node — it does not ",
                "match a substring inside a literal. So to change the text of a string, match ",
                "the whole literal. ",
                "Example: {\"path\": \"src/app.js\", \"ops\": [{\"pat\": ",
                "\"console.log(\\\"$M\\\")\", \"out\": \"console.log([\\\"$M\\'])\"}]}",
                " A successful edit returns the updated \u{00B6}path#TAG header — ",
                "use it directly for follow-up edits on the same file without ",
                "re-reading."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to the project root"
                    },
                    "ops": {
                        "type": "array",
                        "description": "Rewrite ops applied in order",
                        "items": {
                            "type": "object",
                            "properties": {
                                "pat": {
                                    "type": "string",
                                    "description": concat!(
                                        "Complete, structurally valid AST pattern. ",
                                        "Supports metavariables $NAME (exactly one node) ",
                                        "and $$$NAME (zero or more nodes, e.g. an argument ",
                                        "list); the same metavariable must capture identical ",
                                        "text. Include quotes around string literals; a ",
                                        "metavariable matches a whole node, never a substring ",
                                        "inside a literal."
                                    )
                                },
                                "out": {
                                    "type": "string",
                                    "description": "Replacement template; captured metavariables may be referenced"
                                }
                            },
                            "required": ["pat", "out"]
                        }
                    }
                },
                "required": ["path", "ops"]
            }
        })
    }

    /// MCP `fs_edit` description (the content replace engine).
    ///
    /// Follows the Claude Code `Edit` shape the models train on massively
    /// (`{file_path?, old_string, new_string, replace_all?}`), adapted to this
    /// environment's mandatory `¶path#TAG` hashline anchor (`path`/`file_hash`).
    fn description_edit() -> ToolDescription {
        serde_json::json!({
            "name": "fs_edit",
            "description": concat!(
                "Performs exact string replacement in a file: replaces an ",
                "exact `old_string` (which must occur exactly once, unless ",
                "`replace_all`) with `new_string`. Use it when the text ",
                "itself identifies the location, with the smallest unique ",
                "snippet. Copy `old_string` verbatim including whitespace ",
                "and newlines. A successful edit returns the updated ",
                "\u{00B6}path#TAG header — use it directly for follow-up edits ",
                "on the same file without re-reading. ",
                "Example: {\"path\": \"src/main.rs\", \"file_hash\": \"3C4D\", ",
                "\"old_string\": \"old_computation(x)\", \"new_string\": ",
                "\"new_computation(x)\"}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to the project root"
                    },
                    "file_hash": {
                        "type": "string",
                        "description": concat!(
                            "4-hex content hash tag: the \u{00B6}path#TAG anchor ",
                            "from your last read of this file (or a previous ",
                            "edit result's `header`). Copy verbatim. Optional ",
                            "for the first edit of the call: follow-up edits ",
                            "to the same file chain on the fresh tag produced ",
                            "by the previous edit."
                        )
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": concat!(
                            "The text to replace it with (empty deletes the ",
                            "matched text)"
                        )
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": concat!(
                            "Replace all occurrences of old_string ",
                            "(default false)"
                        )
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": concat!(
                            "Preview mode: the edit is applied in memory only and the ",
                            "result carries the unified diff plus the syntax-probe ",
                            "verdict (a warning when the edit would break parsing) ",
                            "without writing anything. Re-issue without dry_run to ",
                            "apply the exact same edit."
                        )
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }
        })
    }

    async fn edit_replace(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let dry_run = args
            .get("dry_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let targets_value = if let Some(t) = args.get("targets") {
            // Batch form: kept for benchmarks, not advertised by the schema.
            t.clone()
        } else if args.get("path").is_some() && args.get("ops").is_some() {
            // Advertised single-path shape: a flat {path, file_hash, ops}.
            serde_json::Value::Array(vec![args.clone()])
        } else {
            return Err(
                "the replace engine requires {path, file_hash, ops} — one file per call"
                    .to_string(),
            );
        };
        let targets: Vec<EditTarget> = serde_json::from_value(targets_value)
            .map_err(|e| format!("invalid `path`/`file_hash`/`ops` for the replace engine: {e}"))?;
        edit(metadata.clone(), FsEdit { targets, dry_run })
            .await
            .map_err(|e| e.to_string())
    }

    /// AST-engine adapter (named distinctly from the public
    /// [`Fs::edit_ast`](Self::edit_ast) wrapper).
    async fn edit_ast_engine(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        // Advertised single-path shape: a flat {ops: [...], path}. The
        // multi-path `paths` array form is kept for codemods/benchmarks but
        // not advertised by the schema.
        let ast_value = if let Some(ast) = args.get("ast") {
            ast.clone()
        } else if args.get("ops").is_some_and(|v| v.is_array()) {
            let mut flat = args.clone();
            if let Some(path) = flat.get("path").cloned()
                && let Some(obj) = flat.as_object_mut()
            {
                obj.insert("paths".into(), serde_json::json!([path]));
            }
            flat
        } else {
            return Err(
                "the AST engine requires {path, ops: [{pat, out}]} — one file per call".to_string(),
            );
        };
        let fs_ast: FsAstEdit = serde_json::from_value(ast_value)
            .map_err(|e| format!("invalid `ops`/`path` arguments for the AST engine: {e}"))?;
        crate::fs::ast_edit::ast_edit(metadata.clone(), fs_ast).await
    }

    async fn edit_content(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        // Advertised single-edit shape: a flat {path, file_hash?, old_string,
        // new_string, replace_all?}. The array/batch form is kept for
        // benchmarks but not advertised by the schema.
        let edits_value = if let Some(arr) = args.get("edits").filter(|v| v.is_array()) {
            arr.clone()
        } else if args.get("old_string").is_some() {
            serde_json::Value::Array(vec![args.clone()])
        } else {
            return Err(
                "the content replace engine requires {path, file_hash?, old_string, \
                 new_string, replace_all?} — one file per call"
                    .to_string(),
            );
        };
        let dry_run = args
            .get("dry_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let fs_edits: FsContentEdit =
            serde_json::from_value(serde_json::json!({ "edits": edits_value, "dry_run": dry_run }))
                .map_err(|e| {
                    format!("invalid edit arguments for the content replace engine: {e}")
                })?;
        replace::content_edit(metadata, &fs_edits.edits, fs_edits.dry_run)
            .await
            .map_err(|e| e.to_string())
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
        self.rollback_op(self.lsp.as_ref(), false, path, hash).await
    }

    async fn rollback_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        path: &str,
        hash: &str,
    ) -> Result<RollbackResult, String> {
        let mut result = rollback(&FsRollback, self.metadata(), path, hash).await?;
        if let Some(lsp) = lsp {
            result.lsp_notes =
                Self::passive_notes(lsp, &self.root, &result.path, include_warnings).await;
        }
        Ok(result)
    }

    /// Ensure the servers covering `rel` are running and the file is open on
    /// them. Passive best-effort: failures are logged, never surfaced.
    async fn warm_lsp(lsp: &Lsp, root: &std::path::Path, rel: &str) {
        let path = root.join(rel);
        if let Ok(handles) = lsp.manager().ensure_for_file(&path).await {
            for handle in &handles {
                if let Err(err) = handle.touch_file(&path).await {
                    log::debug!("lsp warm-up of `{}` failed: {err}", path.display());
                }
            }
        }
    }

    /// Collect structured diagnostics for `rel` after a mutation: ensure the
    /// file's servers are up, wait for the diagnostics to settle, then split
    /// the snapshot by severity. Passive best-effort — any failure degrades
    /// to `None` so the tool result is never rejected because of LSP.
    async fn passive_notes(
        lsp: &Lsp,
        root: &std::path::Path,
        rel: &str,
        include_warnings: bool,
    ) -> Option<LspNotes> {
        let path = root.join(rel);
        let display = std::path::Path::new(rel)
            .strip_prefix(root)
            .unwrap_or(std::path::Path::new(rel))
            .display()
            .to_string();

        let Ok(handles) = lsp.manager().ensure_for_file(&path).await else {
            return None;
        };
        for handle in &handles {
            if let Err(err) = handle.touch_file(&path).await {
                log::debug!(
                    "passive diagnostics: could not open `{}` on {}: {err}",
                    path.display(),
                    handle.name()
                );
            }
            // The fs layer just wrote the file to disk: signal the save so
            // servers with post-save pipelines (rust-analyzer's flycheck)
            // regenerate compile-error diagnostics — didChange alone never
            // triggers them, and the server's own watcher may be broken.
            if let Err(err) = handle.save_file(&path) {
                log::debug!(
                    "passive diagnostics: could not save `{}` on {}: {err}",
                    path.display(),
                    handle.name()
                );
            }
        }

        let engine = lsp.diagnostics_engine();

        // Pull the diagnostics instead of waiting for a push. Servers that
        // see the client advertising the `diagnostic` capability — this one
        // does — may never push (rust-analyzer answers with
        // `workspace/diagnostic/refresh` nudges and goes silent), so the
        // only reliable observation after a mutation is an explicit pull.
        //
        // A server mid-analysis answers `-32802 ServerCancelled`: per the
        // pull contract that means "retry", so each server keeps re-pulling
        // until it delivers, until the shared budget expires, or until it
        // reports a definitive failure. Servers without pull support answer
        // `MethodNotFound` and the push stream remains the source; both
        // paths feed the same engine.
        let pull_deadline = std::time::Instant::now() + LSP_REACTION;
        for handle in &handles {
            loop {
                match handle
                    .pull_diagnostics(&path, Duration::from_secs(LSP_PULL_TIMEOUT_SECS))
                    .await
                {
                    Ok(Some(params)) => {
                        // Ingest under a pull-scoped source: the same server
                        // also pushes flycheck diagnostics (cargo check
                        // errors), and the store replaces per source — a
                        // bare-source ingest would let analysis hints wipe
                        // freshly published compile errors.
                        let pull_source = format!("{}/pull", handle.name());
                        engine.ingest(&pull_source, &params);
                        break;
                    }
                    Ok(None) => break,
                    Err(cosh_sdk::lsp::LspError::Rpc { code: -32802, .. })
                        if std::time::Instant::now() < pull_deadline =>
                    {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    Err(err) => {
                        log::debug!(
                            "passive diagnostics: pull from {} failed: {err}",
                            handle.name()
                        );
                        break;
                    }
                }
            }
        }

        // Give any push-driven burst a quiet window before snapshotting.
        engine.wait_for_settle(LSP_SETTLE).await;

        let mut notes = LspNotes::default();
        for diag in engine.snapshot_for(&path) {
            let note = LspNote {
                path: display.clone(),
                line: diag.range.start.line + 1,
                message: diag.message,
                source: diag.source,
            };
            if diag.severity.is_none_or(|s| s <= DiagnosticSeverity::ERROR) {
                if notes.errors.len() < LSP_MAX_NOTES {
                    notes.errors.push(note);
                }
            } else if diag.severity == Some(DiagnosticSeverity::WARNING)
                && include_warnings
                && notes.warnings.len() < LSP_MAX_NOTES
            {
                notes.warnings.push(note);
            }
        }

        (!notes.errors.is_empty() || !notes.warnings.is_empty()).then_some(notes)
    }

    /// Get the project root path.
    #[must_use]
    pub const fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Get the write allowlist (read-only reference).
    #[must_use]
    pub fn allowlist_ref(&self) -> Option<&[PathBuf]> {
        self.allowlist.as_deref()
    }

    /// Get the write blocklist (read-only reference).
    #[must_use]
    pub fn blocklist_ref(&self) -> Option<&[PathBuf]> {
        self.blocklist.as_deref()
    }

    /// Add a path to the write allowlist.
    ///
    /// If the allowlist is `None`, it is created. Duplicate paths are ignored.
    pub fn add_allowlist_path(&mut self, path: PathBuf) {
        let list = self.allowlist.get_or_insert_with(Vec::new);
        if !list.contains(&path) {
            list.push(path);
        }
    }

    /// Remove a path from the write allowlist (for AllowOnce cleanup).
    ///
    /// If the path is not in the allowlist, this is a no-op.
    pub fn remove_allowlist_path(&mut self, path: &std::path::Path) {
        if let Some(list) = self.allowlist.as_mut() {
            list.retain(|p| p.as_path() != path);
        }
        if let Some(list) = self.read_allowlist.as_mut() {
            list.retain(|p| p.as_path() != path);
        }
    }
}

// Auto-dispatch schema-misuse detection

/// Per-call policy guard returned by [`Fs::without_lsp`] and [`Fs::warnings`].
///
/// The guard carries the policy for exactly the calls made through it; the
/// originating `Fs` keeps its default behavior for every later call.
pub struct FsCall<'a> {
    fs: &'a Fs,
    lsp: Option<&'a Arc<Lsp>>,
    include_warnings: bool,
}

impl FsCall<'_> {
    /// See [`Fs::read`].
    pub async fn read(&self, targets: Vec<Target>) -> Vec<ReadResult> {
        self.fs.read_op(self.lsp, targets).await
    }

    /// See [`Fs::write`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Fs::write`].
    pub async fn write(&self, targets: Vec<TargetFile>) -> Result<Vec<WriteResult>, String> {
        self.fs
            .write_op(self.lsp, self.include_warnings, targets)
            .await
    }

    /// See [`Fs::edit`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Fs::edit`].
    pub async fn edit(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.fs.edit_op(self.lsp, self.include_warnings, args).await
    }

    /// See [`Fs::edit_lines`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Fs::edit_lines`].
    pub async fn edit_lines(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.fs
            .edit_lines_op(self.lsp, self.include_warnings, args)
            .await
    }

    /// See [`Fs::edit_ast`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Fs::edit_ast`].
    pub async fn edit_ast(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.fs
            .edit_ast_op(self.lsp, self.include_warnings, args)
            .await
    }

    /// See [`Fs::rollback`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Fs::rollback`].
    pub async fn rollback(&self, path: &str, hash: &str) -> Result<RollbackResult, String> {
        self.fs
            .rollback_op(self.lsp, self.include_warnings, path, hash)
            .await
    }
}
