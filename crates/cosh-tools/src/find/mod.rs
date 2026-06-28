//! File-search tools built on the `cosh-sdk` find engine.
//!
//! - [`glob::glob`]: find files and directories by glob pattern.
//! - [`grep::grep`]: search file content with a regex.
//!
//! [`Find`] wraps both tools in a single builder — configure shared options
//! once and call either operation.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::find::Find;
//!
//! let f = Find::new()
//!     .recursive(true)
//!     .max_results(20)
//!     .gitignore(true);
//!
//! f.glob("*.rs", "/project/src");
//! f.grep("fn main", "/project/src");
//! ```

pub mod glob;
pub mod grep;
pub mod types;

#[cfg(test)]
mod test;

pub use glob::glob;
pub use grep::grep;
pub use types::{ContextEntry, Glob, GlobEntry, GlobOutput, Grep, GrepMatchEntry, GrepOutput};

use types::{Glob as GlobConfig, Grep as GrepConfig};
use crate::ToolDescription;

/// Shared-state wrapper for file-search tool operations.
///
/// Holds configuration for both [`glob`] and [`grep`] searches. Fields
/// that are not set via builder methods remain `None`, deferring to each
/// tool's internal default.
pub struct Find {
    file_type: Option<String>,
    recursive: Option<bool>,
    max_results: Option<u32>,
    sort_by_mtime: Option<bool>,
    name_glob: Option<String>,
    language: Option<String>,
    ignore_case: Option<bool>,
    max_count: Option<u32>,
    context_before: Option<u32>,
    context_after: Option<u32>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
    timeout_ms: Option<u32>,

    /// MCP Tool description for `glob`.
    pub description_glob: ToolDescription,
    /// MCP Tool description for `grep`.
    pub description_grep: ToolDescription,
}

impl Default for Find {
    fn default() -> Self {
        Self::new()
    }
}

impl Find {
    /// Create a new `Find` with all options unset (tool defaults apply).
    #[must_use]
    pub fn new() -> Self {
        Self {
            file_type: None,
            recursive: None,
            max_results: None,
            sort_by_mtime: None,
            name_glob: None,
            language: None,
            ignore_case: None,
            max_count: None,
            context_before: None,
            context_after: None,
            hidden: None,
            gitignore: None,
            timeout_ms: None,
            description_glob: serde_json::json!({
                "name": "find_glob",
                "description": concat!(
                    "Find files and directories matching a glob pattern. ",
                    "Supports recursive search, file type filtering (file/dir/symlink), ",
                    "sorting by modification time, hidden file inclusion, and ",
                    ".gitignore respect. Returns a list of matching entries with ",
                    "path, file type, size, and modification time."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "The glob pattern to match (e.g. \"**/*.rs\", \"src/**\", \"*.toml\")"
                        },
                        "path": {
                            "type": "string",
                            "description": "The root directory to search within"
                        }
                    },
                    "required": ["pattern", "path"]
                }
            }),
            description_grep: serde_json::json!({
                "name": "find_grep",
                "description": concat!(
                    "Search file content for lines matching a regex pattern. ",
                    "Supports case-insensitive matching, file name glob filtering, ",
                    "language-specific search, context lines before/after each match, ",
                    "max count limiting, hidden file inclusion, and .gitignore respect. ",
                    "Returns match locations with line numbers, content, and context."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "The regex pattern to search for in file contents"
                        },
                        "path": {
                            "type": "string",
                            "description": "The root directory to search within"
                        }
                    },
                    "required": ["pattern", "path"]
                }
            }),
        }
    }

    // Glob-specific

    /// Restrict glob results to a filesystem kind (`"file"`, `"dir"`, `"symlink"`).
    #[must_use]
    pub fn file_type(mut self, ft: impl Into<String>) -> Self {
        self.file_type = Some(ft.into());
        self
    }

    /// Search subdirectories recursively (default: `true`).
    #[must_use]
    pub fn recursive(mut self, v: bool) -> Self {
        self.recursive = Some(v);
        self
    }

    /// Maximum number of glob entries to return.
    #[must_use]
    pub fn max_results(mut self, n: u32) -> Self {
        self.max_results = Some(n);
        self
    }

    /// Sort glob results by modification time, most recent first.
    #[must_use]
    pub fn sort_by_mtime(mut self, v: bool) -> Self {
        self.sort_by_mtime = Some(v);
        self
    }

    // Grep-specific

    /// Restrict grep to files whose names match this glob (e.g. `"*.rs"`).
    #[must_use]
    pub fn name_glob(mut self, g: impl Into<String>) -> Self {
        self.name_glob = Some(g.into());
        self
    }

    /// Restrict grep to files of a given language (e.g. `"rust"`, `"py"`).
    #[must_use]
    pub fn language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    /// Case-insensitive matching.
    #[must_use]
    pub fn ignore_case(mut self, v: bool) -> Self {
        self.ignore_case = Some(v);
        self
    }

    /// Maximum total number of grep matches across all files.
    #[must_use]
    pub fn max_count(mut self, n: u32) -> Self {
        self.max_count = Some(n);
        self
    }

    /// Lines of context to include before each match.
    #[must_use]
    pub fn context_before(mut self, n: u32) -> Self {
        self.context_before = Some(n);
        self
    }

    /// Lines of context to include after each match.
    #[must_use]
    pub fn context_after(mut self, n: u32) -> Self {
        self.context_after = Some(n);
        self
    }

    // Shared

    /// Include hidden files / directories (names starting with `.`).
    #[must_use]
    pub fn hidden(mut self, v: bool) -> Self {
        self.hidden = Some(v);
        self
    }

    /// Respect `.gitignore` rules.
    #[must_use]
    pub fn gitignore(mut self, v: bool) -> Self {
        self.gitignore = Some(v);
        self
    }

    /// Abort the search after this many milliseconds.
    #[must_use]
    pub fn timeout_ms(mut self, ms: u32) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    // Operations

    /// Find filesystem entries matching a glob pattern.
    ///
    /// See [`glob`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error when the search path does not exist, the pattern
    /// is invalid, or the operation times out.
    pub fn glob(&self, pattern: &str, path: &str) -> Result<GlobOutput, String> {
        glob(
            &GlobConfig {
                file_type: self.file_type.clone(),
                recursive: self.recursive,
                max_results: self.max_results,
                sort_by_mtime: self.sort_by_mtime,
                hidden: self.hidden,
                gitignore: self.gitignore,
                timeout_ms: self.timeout_ms,
            },
            pattern,
            path,
        )
    }

    /// Search file content for lines matching a regex pattern.
    ///
    /// See [`grep`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be resolved, the pattern is
    /// an invalid regex, or the operation times out.
    pub fn grep(&self, pattern: &str, path: &str) -> Result<GrepOutput, String> {
        grep(
            &GrepConfig {
                glob: self.name_glob.clone(),
                file_type: self.language.clone(),
                ignore_case: self.ignore_case,
                max_count: self.max_count,
                context_before: self.context_before,
                context_after: self.context_after,
                hidden: self.hidden,
                gitignore: self.gitignore,
                timeout_ms: self.timeout_ms,
            },
            pattern,
            path,
        )
    }
}
