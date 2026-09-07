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
pub mod read;
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
    AstEditOp, EditTarget, FsAstEdit, FsEdit, FsMetadata, FsRead, FsRollback, FsRollbackInput,
    FsWrite, LspNote, LspNotes, Target, TargetFile,
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

/// Which edit engine [`Fs::edit`] dispatches to.
///
/// - [`Auto`](EditEngine::Auto) (default) lets the tool arguments decide: the
///   agent populates exactly one of the two optional arguments (`targets` for
///   the hashline replace engine, `ast` for the AST engine). A misused
///   argument (e.g. AST metavariables inside `targets`) is rejected with a
///   correction prompt.
/// - [`Replace`](EditEngine::Replace) forces the hashline replace engine.
/// - [`Ast`](EditEngine::Ast) forces the AST structural engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditEngine {
    /// Agent-controlled: the tool's arguments select the engine (default).
    #[default]
    Auto,
    /// Force the hashline replace engine ([`edit`]).
    Replace,
    /// Force the AST structural engine ([`ast_edit`]).
    Ast,
}

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

    /// Engine selection used by [`Fs::edit`].
    edit_engine: EditEngine,

    /// Language-server engine for passive diagnostics. Its presence is the
    /// toggle: set once via [`Fs::with_lsp`], every mutating operation then
    /// reports findings on the touched files.
    lsp: Option<Arc<Lsp>>,

    /// MCP Tool description for `read`.
    pub description_read: ToolDescription,
    /// MCP Tool description for `write`.
    pub description_write: ToolDescription,
    /// MCP Tool description for `edit`.
    pub description_edit: ToolDescription,
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
            edit_engine: EditEngine::Auto,
            lsp: None,
            description_read: serde_json::json!({
                "name": "fs_read",
                "description": concat!(
                    "Read one or more files, named symbols, or exact line ranges from ",
                    "the project. Each target can specify: a path with an optional ",
                    "`line` number (reads the syntactic block containing that line), ",
                    "an optional `symbol` name (function, class, variable) to look up, ",
                    "or an optional `line_range` (\"start-end\", 1-based inclusive, ",
                    "comma-separated for multiple disjoint ranges) to read exact ",
                    "lines without AST resolution. `line_range` takes precedence ",
                    "over `line`/`symbol`. Every result carries a \u{00b6}path#TAG ",
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
                    "properties": {
                        "targets": {
                            "type": "array",
                            "description": "List of read targets",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": {
                                        "type": "string",
                                        "description": "Path to the file to read, relative to the project root"
                                    },
                                    "line": {
                                        "type": "integer",
                                        "description": concat!(
                                            "Optional specific 1-based line number to read ",
                                            "(reads the syntactic block containing that line)"
                                        )
                                    },
                                    "symbol": {
                                        "type": "string",
                                        "description": concat!(
                                            "Optional symbol name to look up ",
                                            "(function, class, variable) within the file"
                                        )
                                    },
                                    "line_range": {
                                        "type": "string",
                                        "description": concat!(
                                            "Optional 1-based inclusive line range(s) to read exactly, ",
                                            "e.g. \"50-100\" or \"10-20,200-220\"; takes precedence ",
                                            "over line/symbol and does not use AST block resolution"
                                        )
                                    }
                                },
                                "required": ["path"]
                            }
                        }
                    },
                    "required": ["targets"]
                }
            }),
            description_write: serde_json::json!({
                "name": "fs_write",
                "description": concat!(
                    "Write content to one or more files. Creates new files or ",
                    "overwrites existing ones entirely. Paths are validated against ",
                    "the project root, allowlist, and blocklist guards before writing.\n\n",
                    "IMPORTANT: When overwriting an existing file, you MUST include the ",
                    "`file_hash` from a previous `fs_read` call. This proves you have ",
                    "read the file before overwriting it. If you omit `file_hash` on an ",
                    "existing file, the write will be rejected. For new files (that do ",
                    "not yet exist), `file_hash` is not needed."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "targets": {
                            "type": "array",
                            "description": "List of file write targets",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": {
                                        "type": "string",
                                        "description": "Path to the file to write, relative to the project root"
                                    },
                                    "text": {
                                        "type": "string",
                                        "description": "Full text content to write to the file"
                                    },
                                    "file_hash": {
                                        "type": ["string", "null"],
                                        "description": concat!(
                                            "The hash from `fs_read` of this file. REQUIRED when overwriting ",
                                            "an existing file to prove you have read its current content. ",
                                            "Omit or set to null for new files."
                                        )
                                    }
                                },
                                "required": ["path", "text"]
                            }
                        }
                    },
                    "required": ["targets"]
                }
            }),
            description_edit: Self::description_edit_auto(),
            description_rollback: serde_json::json!({
                "name": "fs_rollback",
                "description": concat!(
                    "Roll back a file to a previously recorded session version. ",
                    "Uses the session's rollback history to restore the file to the ",
                    "state identified by the given hash. Returns an error if the ",
                    "path has no rollback history."
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

    async fn read_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        targets: Vec<Target>,
    ) -> Vec<ReadResult> {
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

    /// Apply file edits, dispatching to the appropriate engine.
    ///
    /// The exact engine depends on [`EditEngine`] (see [`only_ast`](Self::only_ast)
    /// and [`only_replace`](Self::only_replace)):
    ///
    /// - [`Auto`](EditEngine::Auto) (default) inspects the two optional tool
    ///   arguments. Populating `targets` runs the hashline replace engine;
    ///   populating `ast` runs the AST structural engine. Providing neither or
    ///   both, or filling an argument with the *other* engine's schema, returns
    ///   a correction prompt instead of a silent failure.
    /// - [`Replace`](EditEngine::Replace) runs only [`edit`].
    /// - [`Ast`](EditEngine::Ast) runs only [`ast_edit`].
    ///
    /// ## Failure semantics differ by engine
    ///
    /// Both engines abort a multi-file batch at the first failure, but only
    /// the replace engine reports that abort as an explicit chain:
    ///
    /// - **Replace** ([`edit`]): targets are applied in the order the caller
    ///   lists them, so **order carries intention** — the agent's edits may
    ///   build on one another. When target N fails, the batch stops: targets
    ///   before N stay applied and are returned with their fresh hashline
    ///   tags in [`EditBatchError::applied`], and targets after N are
    ///   deliberately skipped ([`EditBatchError::skipped`]) — reported as a
    ///   *consequence* of N failing, never as independent failures.
    /// - **AST** ([`ast_edit`]): files are resolved from `paths` (files,
    ///   directories, globs) and processed in **sorted, deduplicated order**,
    ///   not the caller's order — so order does **not** carry the same
    ///   intention. A failure therefore aborts with a plain error string for
    ///   the failing file; already-rewritten files and the ones that would
    ///   have followed are not surfaced, because there is no caller-intended
    ///   order to preserve.
    ///
    /// # Errors
    ///
    /// Returns an error string when no/edit-engine arguments are invalid, a
    /// correction is needed, or an engine fails.
    pub async fn edit(&self, args: serde_json::Value) -> Result<Vec<EditResult>, String> {
        self.edit_op(self.lsp.as_ref(), false, args).await
    }

    async fn edit_op(
        &self,
        lsp: Option<&Arc<Lsp>>,
        include_warnings: bool,
        args: serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let metadata = self.metadata();
        let mut results = match self.edit_engine {
            EditEngine::Replace => self.edit_replace(&metadata, &args).await?,
            EditEngine::Ast => self.edit_ast(&metadata, &args).await?,
            EditEngine::Auto => self.edit_auto(&metadata, &args).await?,
        };
        if let Some(lsp) = lsp {
            for result in &mut results {
                result.lsp_notes =
                    Self::passive_notes(lsp, &self.root, &result.path, include_warnings).await;
            }
        }
        Ok(results)
    }

    /// Restrict [`edit`](Self::edit) to the hashline replace engine.
    #[must_use]
    pub fn only_replace(mut self) -> Self {
        self.edit_engine = EditEngine::Replace;
        self.description_edit = Self::description_edit_replace();
        self
    }

    /// Restrict [`edit`](Self::edit) to the AST structural engine.
    #[must_use]
    pub fn only_ast(mut self) -> Self {
        self.edit_engine = EditEngine::Ast;
        self.description_edit = Self::description_edit_ast();
        self
    }

    /// Return to the default auto-dispatch behaviour of [`edit`](Self::edit).
    #[must_use]
    pub fn auto(mut self) -> Self {
        self.edit_engine = EditEngine::Auto;
        self.description_edit = Self::description_edit_auto();
        self
    }

    /// Engine selection currently configured for [`edit`](Self::edit).
    #[must_use]
    pub const fn edit_engine(&self) -> EditEngine {
        self.edit_engine
    }

    /// MCP `fs_edit` description for the hashline replace engine only.
    ///
    /// The AST engine is not mentioned at all: when the tool is forced to the
    /// replace engine the agent must not learn that an AST engine exists.
    fn description_edit_replace() -> ToolDescription {
        serde_json::json!({
            "name": "fs_edit",
            "description": concat!(
                "Apply targeted line/block edits to one or more files. Edits are ",
                "anchored by the file's content hash for safety — if the file changed ",
                "since it was read, the tool attempts automatic 3-way merge recovery. ",
                "Supports replace, delete, insert (before/after/head/tail), and ",
                "tree-sitter block operations. Successful edits return the ",
                "updated \u{00B6}path#TAG header — use it directly for follow-up ",
                "edits on the same file without re-reading."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "targets": {
                        "type": "array",
                        "description": "List of edit targets",
                        "items": {
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
                                }
                            },
                            "required": ["path", "file_hash", "ops"]
                        }
                    }
                },
                "required": ["targets"]
            }
        })
    }

    /// MCP `fs` tool description for the AST structural engine only.
    ///
    /// The hashline replace engine is not mentioned at all: when the agent is
    /// forced to the AST engine it must not learn that `targets`/hashline exists.
    fn description_edit_ast() -> ToolDescription {
        serde_json::json!({
            "name": "fs_edit",
            "description": concat!(
                "Apply structural syntax-tree edits to one or more files. ",
                "Rewrite each match of a pattern (`pat`) to a template (`out`) ",
                "across the given `paths`. Patterns must be full, structurally ",
                "valid source snippets; they support metavariables $NAME (matches ",
                "exactly one node) and $$$NAME (matches zero or more nodes, e.g. an ",
                "argument list), and the same metavariable must capture identical ",
                "text in every occurrence to match. A string literal pattern must ",
                "include its quotes, and a metavariable captures a whole node — it ",
                "does not match a substring inside a literal. So to change the text ",
                "of a string, match the whole literal, e.g. `pat` ",
                "`console.log(\"$M\")` for `out` `console.log(\"[$M]\")`; a pattern ",
                "like `Hello, $X!` (a bare phrase) matches nothing."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ast": {
                        "type": "object",
                        "description": "Structural rewrite operations.",
                        "properties": {
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
                            },
                            "paths": {
                                "type": "array",
                                "description": "Files, directories, or globs to rewrite",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["ops", "paths"]
                    }
                },
                "required": ["ast"]
            }
        })
    }

    /// MCP `fs` tool description for the default auto-dispatch mode.
    ///
    /// Both engines are exposed here (and only here): the agent populates
    /// exactly one of `targets` / `ast` and the tool routes accordingly.
    fn description_edit_auto() -> ToolDescription {
        serde_json::json!({
            "name": "fs_edit",
            "description": concat!(
                "Apply targeted edits to one or more files. Two mutually exclusive ",
                "engines are available; provide exactly one of the two optional ",
                "arguments. `targets` uses the hashline replace engine (line/block ",
                "edits anchored by the file's content hash, with 3-way merge ",
                "recovery; supports replace, delete, insert before/after/head/tail, ",
                "and tree-sitter block operations). `ast` uses the AST structural ",
                "engine: it matches a syntax-tree pattern (`pat`) that may use ",
                "metavariables `$NAME` (one node) and `$$$NAME` (a list, e.g. an ",
                "argument list) and rewrites each match to the template (`out`), ",
                "enforcing metavariable identity. `pat` must be a complete, ",
                "structurally valid snippet: string literals need their quotes, and ",
                "a metavariable captures a whole node (never a substring inside a ",
                "literal) — to change a string's text, match the whole literal, ",
                "e.g. `pat` `console.log(\"$M\")` for `out` `console.log(\"[$M]\")`. ",
                "If you only need to replace literal text (no restructuring), prefer ",
                "`targets`. If a populated argument is filled with the other ",
                "engine's schema, a correction is returned. ",
                "Successful edits return the updated \u{00B6}path#TAG header — ",
                "use it directly for follow-up edits on the same file without ",
                "re-reading."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "targets": {
                        "type": "array",
                        "description": "Hashline replace engine argument. List of edit targets. Mutually exclusive with `ast`.",
                        "items": {
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
                                }
                            },
                            "required": ["path", "file_hash", "ops"]
                        }
                    },
                    "ast": {
                        "type": "object",
                        "description": concat!(
                            "AST structural engine argument. Rewrites syntax-tree ",
                            "matches of `ops[].pat` to `ops[].out` across `paths`. ",
                            "Mutually exclusive with `targets`."
                        ),
                        "properties": {
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
                            },
                            "paths": {
                                "type": "array",
                                "description": "Files, directories, or globs to rewrite",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["ops", "paths"]
                    }
                },
                "required": []
            }
        })
    }

    async fn edit_replace(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let Some(targets_value) = args.get("targets") else {
            return Err(
                "no `targets` argument was provided; pass 'targets': [{path, file_hash, ops}]"
                    .to_string(),
            );
        };
        let targets: Vec<EditTarget> = serde_json::from_value(targets_value.clone())
            .map_err(|e| format!("invalid `targets` for the replace engine: {e}"))?;
        edit(metadata.clone(), FsEdit { targets })
            .await
            .map_err(|e| e.to_string())
    }

    async fn edit_ast(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let Some(ast_value) = args.get("ast") else {
            return Err(
                "no `ast` argument was provided; pass 'ast': {ops: [{pat, out}], paths: [...]}"
                    .to_string(),
            );
        };
        let fs_ast: FsAstEdit = serde_json::from_value(ast_value.clone())
            .map_err(|e| format!("invalid `ast` argument for the AST engine: {e}"))?;
        crate::fs::ast_edit::ast_edit(metadata.clone(), fs_ast).await
    }

    async fn edit_auto(
        &self,
        metadata: &FsMetadata,
        args: &serde_json::Value,
    ) -> Result<Vec<EditResult>, String> {
        let has_targets = args
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|a| !a.is_empty());
        let has_ast = args.get("ast").is_some_and(|v| v.is_object());

        match (has_targets, has_ast) {
            (true, false) => {
                if let Some(correction) = ast_schema_in_targets(args) {
                    return Err(correction);
                }
                self.edit_replace(metadata, args).await
            }
            (false, true) => {
                if let Some(correction) = replace_schema_in_ast(args) {
                    return Err(correction);
                }
                self.edit_ast(metadata, args).await
            }
            (true, true) => Err(
                "both `targets` and `ast` were provided; pick exactly one engine per call: \
                 use `targets` (replace) OR `ast` (AST), not both."
                    .to_string(),
            ),
            (false, false) => Err(Self::edit_usage_prompt()),
        }
    }

    /// Correction prompt listing the edit tool's two optional arguments.
    fn edit_usage_prompt() -> String {
        "No edit engine arguments were provided. `edit` accepts exactly one of two \
         optional arguments — each selects an engine:\n\
         • `targets` (array of {path, file_hash, ops}) — hashline replace engine \
         (line/block edits, hash-anchored).\n\
         • `ast` (object {ops: [{pat, out}], paths: [...]}) — AST structural engine \
         (master metavariable rewrites).\n\
         Provide whichever matches the edit you intend."
            .to_string()
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
        self.fs
            .edit_op(self.lsp, self.include_warnings, args)
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

/// True when a string uses ast-grep style metavariables (`$name` / `$$$name`).
fn contains_metavar(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'$' {
                j += 1;
            }
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// Detect when the agent filled the replace `targets` argument with the AST
/// engine's schema. Returns a correction prompt, or `None` when the arguments
/// look like a legitimate replace request.
fn ast_schema_in_targets(args: &serde_json::Value) -> Option<String> {
    let arr = args.get("targets")?.as_array()?;
    for target in arr {
        let obj = target.as_object()?;
        if obj.contains_key("pat") || obj.contains_key("out") || obj.contains_key("paths") {
            return Some(
                "The `targets` argument was populated with the AST engine schema \
                 (found `pat`/`out`/`paths`). AST edits belong in the `ast` argument: \
                 'ast': {ops: [{pat, out}], paths: [...]}."
                    .to_string(),
            );
        }
        if let Some(ops) = obj.get("ops").and_then(serde_json::Value::as_str)
            && contains_metavar(ops)
        {
            return Some(
                "The replace `targets[].ops` contains AST pattern metavariables \
                 (`$`/`$$$`). AST edits belong in the `ast` argument: \
                 'ast': {ops: [{pat, out}], paths: [...]}."
                    .to_string(),
            );
        }
    }
    None
}

/// Detect when the agent filled the AST `ast` argument with the replace
/// engine's schema. Returns a correction prompt, or `None` when the arguments
/// look like a legitimate AST request.
fn replace_schema_in_ast(args: &serde_json::Value) -> Option<String> {
    let ast = args.get("ast")?;
    let obj = ast.as_object()?;
    if obj.contains_key("targets") || obj.contains_key("file_hash") {
        return Some(
            "The `ast` argument was populated with the replace engine schema \
             (found `targets`/`file_hash`). Replace edits belong in the `targets` argument: \
             'targets': [{path, file_hash, ops}]."
                .to_string(),
        );
    }
    if let Some(ops) = obj.get("ops").and_then(serde_json::Value::as_array) {
        for op in ops {
            if let Some(oo) = op.as_object()
                && (oo.contains_key("file_hash") || oo.contains_key("ops"))
            {
                return Some(
                    "The `ast.ops` entries look like hashline replace ops \
                     (found `file_hash`/`ops`). Replace edits belong in the `targets` argument: \
                     'targets': [{path, file_hash, ops}]."
                        .to_string(),
                );
            }
        }
    }
    None
}
