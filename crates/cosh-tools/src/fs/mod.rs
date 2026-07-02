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
//! fs.write(vec![TargetFile { path: "foo.txt".to_string(), text: "hello".to_string() }]).await;
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
pub use types::{
    EditTarget, FsEdit, FsMetadata, FsRead, FsRollback, FsRollbackInput, FsWrite, Target, TargetFile,
};
pub use write::{WriteResult, write};

use crate::ToolDescription;

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
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: PathBuf::new(),
            allowlist: None,
            blocklist: None,
            read_allowlist: None,
            read_blocklist: None,
            description_read: serde_json::json!({
                "name": "fs_read",
                "description": concat!(
                    "Read one or more files or named symbols from the project. ",
                    "Each target can specify a file path, an optional line number to ",
                    "read a single line, or an optional symbol name to look up a ",
                    "specific code symbol (function, class, variable, etc.) within ",
                    "the file."
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
                                            "(reads only that line)"
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
                    "the project root, allowlist, and blocklist guards before writing."
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
                                    }
                                },
                                "required": ["path", "text"]
                            }
                        }
                    },
                    "required": ["targets"]
                }
            }),
            description_edit: serde_json::json!({
                "name": "fs_edit",
                "description": concat!(
                    "Apply targeted edits to one or more files using a diff-like ",
                    "instruction format. Uses the file's current hash for safety -- ",
                    "the operation fails if the file has changed since the hash was ",
                    "recorded. Supports semantic operations like replace, insert, ",
                    "and delete on specific text within the file."
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
                                            "Hash of the current file content for ",
                                            "safety verification"
                                        )
                                    },
                                    "ops": {
                                        "type": "string",
                                        "description": concat!(
                                            "Edit operations string describing the changes ",
                                            "to apply (search/replace format)"
                                        )
                                    }
                                },
                                "required": ["path", "file_hash", "ops"]
                            }
                        }
                    },
                    "required": ["targets"]
                }
            }),
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
    /// See [`read`] for details.
    pub async fn read(&self, targets: Vec<Target>) -> Vec<ReadResult> {
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
        write(self.metadata(), FsWrite { targets }).await
    }

    /// Apply edits to one or more files.
    ///
    /// See [`edit`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if a file hash doesn't match, edit operations fail
    /// to parse, or write permissions are denied.
    pub async fn edit(&self, targets: Vec<EditTarget>) -> Result<Vec<EditResult>, String> {
        edit(self.metadata(), FsEdit { targets }).await
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
