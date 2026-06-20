//! Input and output types shared by all `find` tools.

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
pub struct ContextEntry {
    /// 1-indexed line number in the source file.
    pub line_number: u32,
    /// Raw line content.
    pub line: String,
}

/// A single match found by the grep tool.
pub struct GrepMatchEntry {
    /// File path where the match was found.
    pub path: String,
    /// 1-indexed line number of the matched line.
    pub line_number: u32,
    /// Content of the matched line.
    pub line: String,
    /// Context lines immediately before the match.
    pub context_before: Vec<ContextEntry>,
    /// Context lines immediately after the match.
    pub context_after: Vec<ContextEntry>,
}

/// Result returned by the [`grep`](super::grep::grep) tool.
pub struct GrepOutput {
    /// All matches found, ordered by file path.
    pub matches: Vec<GrepMatchEntry>,
    /// Total number of matches across all files.
    pub total_matches: u32,
    /// Number of files that contained at least one match.
    pub files_with_matches: u32,
    /// Number of files searched.
    pub files_searched: u32,
}
