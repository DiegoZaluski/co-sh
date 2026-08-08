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

pub use glob::{GlobMatchCallback, GlobTargetSpec, glob, glob_targets_with, glob_with};
pub use grep::{GrepMatchCallback, grep, grep_targets, grep_targets_with};
pub use types::{
    ContextEntry, Glob, GlobCallOptions, GlobEntry, GlobInput, GlobOutput, Grep, GrepFileEntry,
    GrepInput, GrepMatchEntry, GrepOutput,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Deepest common ancestor directory of the given (existing) paths. A file
/// target contributes its parent directory. Returns `None` only for an empty
/// input.
///
/// Note: when the targets share NO common ancestor (e.g. different mount
/// points), the rebase falls back to each target's raw relative paths, which
/// can collide across targets. On a single-`/` filesystem every absolute path
/// shares `/`, so this only matters for exotic layouts.
pub(crate) fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut ancestor = if first.is_file() {
        first.parent()?.to_path_buf()
    } else {
        first.clone()
    };
    for path in iter {
        while !path.starts_with(&ancestor) {
            if !ancestor.pop() {
                return None;
            }
        }
    }
    Some(ancestor)
}

use crate::ToolDescription;
use crate::util::path_guard::PathGuard;
use glob::parse_find_pattern;
use types::Grep as GrepConfig;

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
    format: Option<String>,
    name_glob: Option<String>,
    language: Option<String>,
    ignore_case: Option<bool>,
    max_count: Option<u32>,
    context_before: Option<u32>,
    context_after: Option<u32>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
    timeout_ms: Option<u32>,

    /// Centralized path guard for path-validation.
    guard: PathGuard,

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
            guard: PathGuard::new(Path::new(""), None, None),
            timeout_ms: None,
            format: None,
            description_glob: serde_json::json!({
                "name": "find_glob",
                "description": concat!(
                    "Find files and directories matching a glob pattern. ",
                    "Search one or more roots in a single call: `paths` (array) ",
                    "overrides `path`; each entry is a root, glob, or literal ",
                    "file/directory (e.g. \"src/**/*.rs\", \"*.rs\", or \"src\"). ",
                    "Literal directories are searched recursively; a bare glob (no ",
                    "directory prefix) is matched recursively across its root; a ",
                    "glob WITH a directory prefix (\"src/*.rs\") stays scoped to ",
                    "that directory (shallow). Missing targets are skipped with a ",
                    "warning. Zero matches are marked `useless` with a `note` ",
                    "(adjust the pattern or scope, do not retry blindly). A ",
                    "timeout returns the partial results with `timed_out: true` — ",
                    "an empty timed-out result is an INCOMPLETE scan, NOT proof of ",
                    "absence; scope to a deeper directory instead of retrying. ",
                    "Results are capped at 200 by default; the optional ",
                    "`max_results` only LOWERS that cap (a value above 200 ",
                    "is clamped to 200). ",
                    "When the cap cuts the list, `limit_reached: true` means more ",
                    "entries may exist. `format` renders the `formatted` ",
                    "field as \"flat\", \"grouped\" (per-directory headers), or ",
                    "\"tree\" (indented). Each entry carries path, file type, ",
                    "size, and modification time."
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
                            "description": "A search root, glob, or literal file/directory. May be relative; CWD-relative results are returned. Omitted: defaults to the working directory."
                        },
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Multiple search roots/globs in one call; overrides `path`. Each target is validated individually; missing targets are skipped with a warning."
                        },
                        "file_type": {
                            "type": "string",
                            "enum": ["file", "dir", "symlink"],
                            "description": "Restrict results to one filesystem kind. Omitted: return files, dirs, and symlinks mixed."
                        },
                        "hidden": {
                            "type": "boolean",
                            "description": "Include hidden files/directories (names starting with `.`). Default true; `.git` is ALWAYS excluded regardless."
                        },
                        "gitignore": {
                            "type": "boolean",
                            "description": "Respect .gitignore rules. Default true; pass false to include ignored files explicitly."
                        },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 200,
                            "description": "Maximum results to return. Defaults to 200; the ceiling is 200, so a larger value only clamps down to it. Use it to shrink, never to request more."
                        },
                        "format": {
                            "type": "string",
                            "enum": ["flat", "grouped", "tree"],
                            "description": "Output layout for the `formatted` field (default \"flat\")"
                        },
                        "sort_by_mtime": {
                            "type": "boolean",
                            "description": "Sort results by modification time, most recent first. Default true."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Abort the search after this many milliseconds; partial results are returned with `timed_out: true`."
                        }
                    },
                    "required": ["pattern"]
                }
            }),
            description_grep: serde_json::json!({
                "name": "find_grep",
                "description": concat!(
                    "Search file content for lines matching a regex pattern. ",
                    "Multi-file searches surface at most 20 distinct files per call ",
                    "(per-file match cap 20); a single-file scope surfaces up to 200 ",
                    "matches. When more files matched, `file_limit_reached` is true and ",
                    "`note` suggests paginating with `skip` (call again with the same ",
                    "pattern/path plus skip=<N> for the next page). Lines longer than ",
                    "200 characters are truncated with a `...` suffix and flagged via ",
                    "`truncated` — read the file for the full line. Patterns containing ",
                    "a newline (or the `\\n` escape) automatically enable multiline ",
                    "matching. Zero selected matches are marked `useless` with a `note` ",
                    "— adjust the pattern or scope instead of blindly retrying. A ",
                    "timeout returns the partial matches with `timed_out: true` — an ",
                    "empty timed-out result is an INCOMPLETE scan, NOT proof of ",
                    "absence; narrow the scope instead of retrying blindly. Search ",
                    "several targets in one call with `paths` (each validated ",
                    "individually; when absent, `path` is used). Restrict to a 1-based ",
                    "inclusive line range with `line_range` (\"start-end\", requires ",
                    "single-file targets). Each shown file carries a hashline anchor in ",
                    "the `files` array (path, file_hash, header like \u{00b6}path#TAG). ",
                    "To edit a matched file directly, pass the header's absolute path ",
                    "and the tag (file_hash) to fs_edit — no re-read is needed to ",
                    "obtain the current hash. Files beyond the anchor window have no ",
                    "entry."
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
                        },
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Multiple search targets (files or directories) in one call; overrides `path`. Each target is validated individually."
                        },
                        "line_range": {
                            "type": "string",
                            "description": "1-based inclusive line range \"start-end\" (requires single-file targets)"
                        },
                        "skip": {
                            "type": "number",
                            "description": "Files to skip before collecting results — paginate when the prior call hit the file window limit"
                        }
                    },
                    "required": ["pattern"]
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
    pub const fn recursive(mut self, v: bool) -> Self {
        self.recursive = Some(v);
        self
    }

    /// Maximum number of glob entries to return.
    #[must_use]
    pub const fn max_results(mut self, n: u32) -> Self {
        self.max_results = Some(n);
        self
    }

    /// Sort glob results by modification time, most recent first.
    #[must_use]
    pub const fn sort_by_mtime(mut self, v: bool) -> Self {
        self.sort_by_mtime = Some(v);
        self
    }

    /// Output layout for glob results (`"flat"`, `"grouped"`, `"tree"`).
    #[must_use]
    pub fn glob_format(mut self, f: impl Into<String>) -> Self {
        self.format = Some(f.into());
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
    pub const fn ignore_case(mut self, v: bool) -> Self {
        self.ignore_case = Some(v);
        self
    }

    /// Maximum total number of grep matches across all files.
    #[must_use]
    pub const fn max_count(mut self, n: u32) -> Self {
        self.max_count = Some(n);
        self
    }

    /// Lines of context to include before each match.
    #[must_use]
    pub const fn context_before(mut self, n: u32) -> Self {
        self.context_before = Some(n);
        self
    }

    /// Lines of context to include after each match.
    #[must_use]
    pub const fn context_after(mut self, n: u32) -> Self {
        self.context_after = Some(n);
        self
    }

    // Shared

    /// Include hidden files / directories (names starting with `.`).
    #[must_use]
    pub const fn hidden(mut self, v: bool) -> Self {
        self.hidden = Some(v);
        self
    }

    /// Respect `.gitignore` rules.
    #[must_use]
    pub const fn gitignore(mut self, v: bool) -> Self {
        self.gitignore = Some(v);
        self
    }

    /// Abort the search after this many milliseconds.
    #[must_use]
    pub const fn timeout_ms(mut self, ms: u32) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    // Security guards

    /// Set the project root directory (used for path-validation guards).
    #[must_use]
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.guard = PathGuard::new(&path.into(), self.guard.allowlist(), self.guard.blocklist());
        self
    }

    /// Set the explicit path allowlist.
    #[must_use]
    pub fn allowlist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        let list: Vec<PathBuf> = paths.into_iter().map(Into::into).collect();
        self.guard = PathGuard::new(self.guard.root(), Some(&list), self.guard.blocklist());
        self
    }

    /// Set the explicit path blocklist.
    #[must_use]
    pub fn blocklist(mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        let list: Vec<PathBuf> = paths.into_iter().map(Into::into).collect();
        self.guard = PathGuard::new(self.guard.root(), self.guard.allowlist(), Some(&list));
        self
    }

    /// Add a path to the allowlist (for session-level persistence).
    pub fn add_allowlist_path(&mut self, path: PathBuf) {
        self.guard.add_allowlist_path(path);
    }

    /// Remove a path from the allowlist (for AllowOnce cleanup).
    ///
    /// If the path is not in the allowlist, this is a no-op.
    pub fn remove_allowlist_path(&mut self, path: &Path) {
        self.guard.remove_allowlist_path(path);
    }

    /// Get the project root path.
    #[must_use]
    pub const fn find_root(&self) -> &PathBuf {
        self.guard.root()
    }

    /// Get the allowlist (read-only reference).
    #[must_use]
    pub fn allowlist_ref(&self) -> Option<&[PathBuf]> {
        self.guard.allowlist()
    }

    /// Get the blocklist (read-only reference).
    #[must_use]
    pub fn blocklist_ref(&self) -> Option<&[PathBuf]> {
        self.guard.blocklist()
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
        self.glob_with(pattern, Some(path.to_string()), None, None)
    }

    /// Like [`glob`](Self::glob), but searches one or more targets in a single
    /// call (`paths` overrides `path`; each target is guard-resolved
    /// individually), optionally streaming matches live via `on_match`.
    ///
    /// Each entry in `path`/`paths` may be a directory, a literal file, or a
    /// glob (with or without a directory prefix); the effective pattern and
    /// recursion are derived from its shape. Output paths are rebased to the
    /// CWD when they live under it (feature: relative-to-CWD resolution).
    ///
    /// # Errors
    ///
    /// Returns an error when the search paths are missing, a pattern is
    /// invalid, or the operation times out.
    pub fn glob_with(
        &self,
        pattern: &str,
        path: Option<String>,
        paths: Option<Vec<String>>,
        on_match: Option<Arc<GlobMatchCallback>>,
    ) -> Result<GlobOutput, String> {
        self.glob_full(pattern, path, paths, GlobCallOptions::default(), on_match)
    }

    /// Like [`glob_with`](Self::glob_with), with the schema-driven call options
    /// bundled in [`GlobCallOptions`]: optional `file_type` (`"file"`,
    /// `"dir"`, `"symlink"`), `hidden` and `gitignore` toggles, `format`
    /// (`"flat"`, `"grouped"`, `"tree"`), a `sort_by_mtime` toggle, an optional
    /// `timeout_ms`, and an optional `max_results`.
    ///
    /// The effective result cap defaults to 200 (mirroring the reference
    /// tool) when neither this call's `max_results` nor the builder's is
    /// set, and the ceiling is fixed: a `max_results` above 200 is clamped
    /// down, so the caller can only lower the result set. A `max_results`
    /// of `0` is rejected.
    ///
    /// `hidden` and `gitignore` default to `true` unless explicitly disabled
    /// (reference-tool behavior). `sort_by_mtime` defaults to `true` —
    /// the most recently modified files surface first. `file_type` filters
    /// results to one filesystem kind and is an extension over the
    /// reference tool.
    ///
    /// # Errors
    ///
    /// Returns an error when a search path cannot be resolved, a pattern is
    /// invalid, `file_type` is unknown, `max_results` is zero, or the
    /// operation times out.
    pub fn glob_full(
        &self,
        pattern: &str,
        path: Option<String>,
        paths: Option<Vec<String>>,
        opts: GlobCallOptions,
        on_match: Option<Arc<GlobMatchCallback>>,
    ) -> Result<GlobOutput, String> {
        let GlobCallOptions {
            file_type,
            hidden,
            gitignore,
            max_results,
            format,
            sort_by_mtime,
            timeout_ms,
        } = opts;
        if let Some(0) = max_results {
            return Err("max_results must be a positive number".to_string());
        }
        let max_results = max_results
            .or(self.max_results)
            .unwrap_or(glob::DEFAULT_GLOB_LIMIT)
            .min(glob::MAX_GLOB_LIMIT);
        // Model-facing defaults mirror the reference tool (oh-my-pi): hidden
        // and gitignore are ON unless explicitly disabled. `file_type` is an
        // optional extension. Each flag falls back to the builder-configured
        // value first.
        let file_type = file_type.or_else(|| self.file_type.clone());
        let hidden = hidden.or(self.hidden).unwrap_or(true);
        let gitignore = gitignore.or(self.gitignore).unwrap_or(true);
        let raw_targets: Vec<String> = match paths {
            Some(list) if !list.is_empty() => list,
            // No explicit root: default to the workspace root (CWD) so a bare
            // pattern never needs a `path`.
            _ => vec![path.unwrap_or_else(|| ".".to_string())],
        };
        // Each raw entry becomes one resolved spec. Guard resolution keeps the
        // existing security contract; the parse step decides directory/glob/
        // file semantics and effective recursion.
        let resolved: Vec<GlobTargetSpec> = {
            let mut resolved: Vec<GlobTargetSpec> = Vec::with_capacity(raw_targets.len());
            for target in &raw_targets {
                let parsed = parse_find_pattern(target);
                let base = self.guard.resolve(&parsed.base_path.to_string_lossy())?;
                resolved.push(GlobTargetSpec {
                    base_path: base,
                    pattern: if parsed.has_glob {
                        parsed.glob_pattern
                    } else {
                        pattern.to_string()
                    },
                    has_glob: parsed.has_glob,
                });
            }
            resolved
        };
        let cwd = self.guard.root().clone();
        glob_targets_with(
            &Glob {
                file_type,
                recursive: self.recursive,
                max_results: Some(max_results),
                sort_by_mtime: sort_by_mtime.or(self.sort_by_mtime),
                hidden: Some(hidden),
                gitignore: Some(gitignore),
                timeout_ms: timeout_ms.or(self.timeout_ms),
                format: format.or_else(|| self.format.clone()),
            },
            &resolved,
            on_match,
            Some(&cwd),
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
        self.grep_with(pattern, Some(path.to_string()), None, None, None)
    }

    /// Like [`grep`](Self::grep), but pages past the first file window with
    /// `skip` (files to skip before collecting results). Ignored for
    /// single-file scopes.
    pub fn grep_skipping(
        &self,
        pattern: &str,
        path: &str,
        skip: Option<u32>,
    ) -> Result<GrepOutput, String> {
        self.grep_with(pattern, Some(path.to_string()), None, skip, None)
    }

    /// Search one or more targets in a single call, with optional pagination
    /// (`skip`) and a 1-based inclusive `line_range` (`"start-end"`).
    ///
    /// `paths` overrides `path` when present; each target is resolved and
    /// validated individually by the path guard, so the approval shown to the
    /// user is exactly the set of targets the search opens. `line_range`
    /// requires every target to be a single file.
    ///
    /// # Errors
    ///
    /// Returns an error when a target cannot be resolved, no target is
    /// provided, the pattern is an invalid regex, or the operation times out.
    pub fn grep_with(
        &self,
        pattern: &str,
        path: Option<String>,
        paths: Option<Vec<String>>,
        skip: Option<u32>,
        line_range: Option<String>,
    ) -> Result<GrepOutput, String> {
        self.grep_with_streaming(pattern, path, paths, skip, line_range, None)
    }

    /// Like [`grep_with`](Self::grep_with), optionally streaming each match
    /// live via `on_match` (formatted `path:line`, rebased for multi-target
    /// calls).
    pub fn grep_with_streaming(
        &self,
        pattern: &str,
        path: Option<String>,
        paths: Option<Vec<String>>,
        skip: Option<u32>,
        line_range: Option<String>,
        on_match: Option<Arc<GrepMatchCallback>>,
    ) -> Result<GrepOutput, String> {
        let raw_targets: Vec<String> = match paths {
            Some(list) if !list.is_empty() => list,
            _ => vec![path.ok_or_else(|| "missing 'path' or 'paths'".to_string())?],
        };
        let mut resolved: Vec<String> = Vec::with_capacity(raw_targets.len());
        for target in &raw_targets {
            let validated = self.guard.resolve(target)?;
            resolved.push(validated.to_string_lossy().to_string());
        }
        grep_targets_with(
            &GrepConfig {
                glob: self.name_glob.clone(),
                file_type: self.language.clone(),
                ignore_case: self.ignore_case,
                max_count: self.max_count,
                skip,
                line_range,
                context_before: self.context_before,
                context_after: self.context_after,
                hidden: self.hidden,
                gitignore: self.gitignore,
                timeout_ms: self.timeout_ms,
            },
            pattern,
            &resolved,
            on_match,
        )
    }
}
