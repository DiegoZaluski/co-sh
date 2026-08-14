//! Input and output types shared by all `find` tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for `find_glob`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GlobInput {
    /// The glob pattern to match (e.g. "**/*.rs", "src/**", "*.toml").
    pub pattern: String,
    /// The root directory to search within. Omitted: defaults to the working
    /// directory.
    pub path: Option<String>,
    /// Multiple search roots in one call. When present, overrides `path`.
    /// Each target is validated individually by the path guard; missing
    /// targets are skipped with a warning instead of failing the whole call.
    pub paths: Option<Vec<String>>,
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
    /// Sort results by modification time, most recent first (default: `true`).
    pub sort_by_mtime: Option<bool>,
    /// Output layout for the `formatted` field: `"flat"`, `"grouped"`, or
    /// `"tree"`. When unset, no formatted rendering is attached.
    pub format: Option<String>,
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
    /// Include hidden files and directories whose names start with `.`.
    /// Left `None`, the free function falls through to the SDK default
    /// (`false`); the `Find` wrapper defaults this to `true` (reference-tool
    /// behavior). The walker ALWAYS skips `.git` regardless.
    pub hidden: Option<bool>,
    /// Maximum number of entries to return.
    pub max_results: Option<u32>,
    /// Respect `.gitignore` rules (default: `true`).
    pub gitignore: Option<bool>,
    /// Sort results by modification time, most recent first (default: `true`).
    pub sort_by_mtime: Option<bool>,
    /// Output layout (`"flat"`, `"grouped"`, `"tree"`).
    pub format: Option<String>,
    /// Abort the search after this many milliseconds.
    pub timeout_ms: Option<u32>,
}

/// Schema-driven call options bundled for the `find_glob` entry point,
/// keeping `glob_full` argument count low as the schema grows.
///
/// `file_type` filters results to one filesystem kind (`"file"`, `"dir"`,
/// `"symlink"`) and is an extension over the reference tool (oh-my-pi).
/// `hidden` and `gitignore` default to `true` (reference behavior) when
/// omitted; `max_results` defaults to 200 with a hard ceiling of 200.
#[derive(Clone, Debug, Default)]
pub struct GlobCallOptions {
    /// Restrict results to a filesystem kind: `"file"`, `"dir"`, or `"symlink"`.
    pub file_type: Option<String>,
    /// Whether to include hidden entries. Omitted defaults to `true`.
    pub hidden: Option<bool>,
    /// Whether to respect `.gitignore`. Omitted defaults to `true`.
    pub gitignore: Option<bool>,
    /// Maximum number of entries to return (clamped to 200).
    pub max_results: Option<u32>,
    /// Output layout: `"flat"`, `"grouped"`, or `"tree"`.
    pub format: Option<String>,
    /// Sort results by modification time, most recent first. Omitted defaults
    /// to `true` (the most recently edited files surface first).
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
    /// `true` when the search hit `max_results` and more entries may exist.
    pub limit_reached: Option<bool>,
    /// `true` when the scan was cut short by the timeout; `matches` holds the
    /// partial results found up to that point. An empty `matches` with this
    /// flag is an INCOMPLETE scan, not proof of absence.
    pub timed_out: Option<bool>,
    /// Human-readable hint for the caller (timeout guidance, no-match notice,
    /// pagination/limit notes).
    pub note: Option<String>,
    /// `true` when no matches were selected. Such a result carries no new
    /// information — adjust the pattern or scope instead of blindly retrying.
    pub useless: Option<bool>,
    /// User-supplied targets whose directory was missing on disk. These were
    /// skipped; the surviving targets' results are still returned.
    pub missing_paths: Option<Vec<String>>,
    /// Matched paths rendered in the requested `format` (`flat`/`grouped`/
    /// `tree`), OR the plain newline-joined paths when no format was
    /// requested. Always present (even for empty results) so the model never
    /// needs to reconstruct grouping from the structured entries.
    pub formatted: String,
    /// The directory this search was scoped to, in the same relative form as
    /// the match paths (`.`, `src`, `crates/…`).
    pub scope: String,
    /// Working directory the paths are relative to. Lets the TUI renderer
    /// resolve match paths to absolute paths for OSC 8 file hyperlinks.
    pub cwd: Option<String>,
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
    /// `true` when the search was cut short by the timeout; `matches` holds
    /// the partial results found up to that point. An empty `matches` with
    /// this flag is an INCOMPLETE search, not proof of absence.
    pub timed_out: Option<bool>,
}
