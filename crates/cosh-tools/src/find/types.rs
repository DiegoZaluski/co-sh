//! Input and output types shared by all `find` tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for `find_glob`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GlobInput {
    /// The glob pattern to match (e.g. "**/*.rs", "src/**", "*.toml").
    pub pattern: String,
    /// The root directory to search within.
    pub path: String,
    /// Restrict results to a filesystem kind: `"file"`, `"dir"`, or `"symlink"`.
    pub file_type: Option<String>,
    /// Search subdirectories recursively (default: `true`).
    pub recursive: Option<bool>,
    /// Include hidden files and directories whose names start with `.` (default: `false`).
    pub hidden: Option<bool>,
    /// Maximum number of entries to return.
    pub max_results: Option<u32>,
    /// Respect `.gitignore` rules (default: `true`).
    pub gitignore: Option<bool>,
    /// Sort results by modification time, most recent first (default: `false`).
    pub sort_by_mtime: Option<bool>,
    /// Abort the search after this many milliseconds.
    pub timeout_ms: Option<u32>,
}

/// Parameters for `find_grep`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepInput {
    /// The regex pattern to search for in file contents.
    pub pattern: String,
    /// The root directory to search within.
    pub path: String,
    /// Multiple search targets (files or directories) to search in a single
    /// call. When present, overrides `path`. Each target is validated
    /// individually by the path guard.
    pub paths: Option<Vec<String>>,
    /// Restrict matches to a 1-based inclusive line range `"start-end"`.
    /// Requires every target to be a single file.
    pub line_range: Option<String>,
    /// Restrict the search to files whose names match this glob (e.g., `"*.rs"`).
    pub glob: Option<String>,
    /// Restrict the search to files of a given language type (e.g., `"rust"`, `"py"`, `"js"`).
    pub file_type: Option<String>,
    /// Case-insensitive matching (default: `false`).
    pub ignore_case: Option<bool>,
    /// Maximum total number of matches to return across all files.
    pub max_count: Option<u32>,
    /// Files to skip before collecting results — use to paginate when the
    /// prior call hit the file window limit.
    pub skip: Option<u32>,
    /// Lines of context to include before each match.
    pub context_before: Option<u32>,
    /// Lines of context to include after each match.
    pub context_after: Option<u32>,
    /// Include hidden files (default: `true`).
    pub hidden: Option<bool>,
    /// Respect `.gitignore` rules (default: `true`).
    pub gitignore: Option<bool>,
    /// Abort the search after this many milliseconds.
    pub timeout_ms: Option<u32>,
}

/// Configuration for the [`glob`](super::glob::glob) tool.
///
/// Fields are optional — use `Glob::default()` for sensible defaults.
#[derive(Default)]
pub struct Glob {
    /// Restrict results to a filesystem kind: `"file"`, `"dir"`, or `"symlink"`.
    pub file_type: Option<String>,
    /// Search subdirectories recursively (default: `true`).
    pub recursive: Option<bool>,
    /// Include hidden files and directories whose names start with `.` (default: `false`).
    pub hidden: Option<bool>,
    /// Maximum number of entries to return.
    pub max_results: Option<u32>,
    /// Respect `.gitignore` rules (default: `true`).
    pub gitignore: Option<bool>,
    /// Sort results by modification time, most recent first (default: `false`).
    pub sort_by_mtime: Option<bool>,
    /// Abort the search after this many milliseconds.
    pub timeout_ms: Option<u32>,
}

/// A single filesystem entry matched by a glob search.
#[derive(Serialize)]
pub struct GlobEntry {
    /// Path relative to the search root, using forward slashes.
    pub path: String,
    /// Filesystem kind: `"file"`, `"dir"`, or `"symlink"`.
    pub file_type: String,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ms: Option<f64>,
    /// File size in bytes (`None` for directories and symlinks).
    pub size_bytes: Option<f64>,
}

/// Result returned by the [`glob`](super::glob::glob) tool.
#[derive(Serialize)]
pub struct GlobOutput {
    /// Matched filesystem entries.
    pub matches: Vec<GlobEntry>,
    /// Total number of entries returned.
    pub total: u32,
}

/// Configuration for the [`grep`](super::grep::grep) tool.
///
/// Fields are optional — use `Grep::default()` for sensible defaults.
#[derive(Default)]
pub struct Grep {
    /// Restrict the search to files whose names match this glob (e.g., `"*.rs"`).
    pub glob: Option<String>,
    /// Restrict the search to files of a given language type (e.g., `"rust"`, `"py"`, `"js"`).
    pub file_type: Option<String>,
    /// Case-insensitive matching (default: `false`).
    pub ignore_case: Option<bool>,
    /// Maximum total number of matches to return across all files.
    pub max_count: Option<u32>,
    /// Files to skip before collecting results — use to paginate when the
    /// prior call hit the file window limit.
    pub skip: Option<u32>,
    /// Restrict matches to a 1-based inclusive line range `"start-end"`.
    /// Requires every target to be a single file.
    pub line_range: Option<String>,
    /// Lines of context to include before each match.
    pub context_before: Option<u32>,
    /// Lines of context to include after each match.
    pub context_after: Option<u32>,
    /// Include hidden files (default: `true`).
    pub hidden: Option<bool>,
    /// Respect `.gitignore` rules (default: `true`).
    pub gitignore: Option<bool>,
    /// Abort the search after this many milliseconds.
    pub timeout_ms: Option<u32>,
}

/// A context line adjacent to a grep match.
#[derive(Clone, Serialize)]
pub struct ContextEntry {
    /// 1-indexed line number in the source file.
    pub line_number: u32,
    /// Raw line content.
    pub line: String,
}

/// A single match found by the grep tool.
#[derive(Clone, Serialize)]
pub struct GrepMatchEntry {
    /// File path where the match was found.
    pub path: String,
    /// 1-indexed line number of the matched line.
    pub line_number: u32,
    /// Content of the matched line.
    pub line: String,
    /// `true` when the line was truncated to the column limit (see the tool
    /// description) — read the file for the full line.
    pub truncated: Option<bool>,
    /// Context lines immediately before the match.
    pub context_before: Vec<ContextEntry>,
    /// Context lines immediately after the match.
    pub context_after: Vec<ContextEntry>,
}

/// Hashline anchor for a single file surfaced by the [`grep`](super::grep::grep) tool.
///
/// Lets the agent edit a file directly from grep output: pair a match's `path`
/// with the matching `file_hash`/`header` and pass them to `fs_edit` without
/// re-reading the file to obtain the current hashline tag.
#[derive(Serialize)]
pub struct GrepFileEntry {
    /// File path, in the same form as the match paths in this result.
    pub path: String,
    /// 4-hex content hash of the whole file — the `file_hash` for `fs_edit`.
    pub file_hash: String,
    /// Hashline header `¶path#TAG` anchoring edits to this file. The path is
    /// absolute so it round-trips through `fs_edit`'s path validation.
    pub header: String,
}

/// Result returned by the [`grep`](super::grep::grep) tool.
#[derive(Serialize)]
pub struct GrepOutput {
    /// All matches found, ordered by file path. Bounded by the file window and
    /// per-file caps described on the tool; page further files with `skip`.
    pub matches: Vec<GrepMatchEntry>,
    /// Total number of matches across all files (a lower bound when caps
    /// trimmed the fetched matches).
    pub total_matches: u32,
    /// Number of files that contained at least one match.
    pub files_with_matches: u32,
    /// Number of files searched.
    pub files_searched: u32,
    /// `true` when more files matched than the shown window — page with `skip`.
    pub file_limit_reached: bool,
    /// `true` when at least one file had more matches than shown (capped to
    /// keep a single hot file from crowding out diverse hits).
    pub per_file_limit_reached: bool,
    /// Human-readable hint: a pagination instruction when the file window was
    /// hit, or a no-match notice (`"No matches found"` / `"No more results …"`).
    pub note: Option<String>,
    /// `true` when no matches were selected. Such a result carries no new
    /// information — adjust the pattern or scope instead of blindly retrying.
    pub useless: Option<bool>,
    /// Hashline anchors for shown files, in encounter order. Bounded to a
    /// small window of files (whole-file tags require reading each file);
    /// files beyond the window surface plain, headerless output.
    pub files: Vec<GrepFileEntry>,
}
