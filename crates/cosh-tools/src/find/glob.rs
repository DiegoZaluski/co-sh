//! Glob-based filesystem discovery tool.
//!
//! Searches one or more roots for entries matching a glob pattern. Results can
//! be filtered by filesystem kind, sorted by modification time, and bounded by
//! a result count or a timeout.
//!
//! # Output contract
//!
//! - A timeout is not an error: the partial matches found up to that point are
//!   returned with `timed_out: true` and an explanatory `note`. An empty
//!   `timed_out` result is an INCOMPLETE scan, never "no files found".
//! - Zero matches (no timeout) are marked `useless: true` with a no-match
//!   `note` — the result carries no new information.
//! - When `max_results` caps the output, `limit_reached: true` tells the
//!   caller more entries may exist.
//! - Multi-target calls skip missing targets and report them in
//!   `missing_paths`; the call only fails when every target is missing.
//! - `on_match` streams each match as the scan finds it (live TUI feedback).

// Pattern and path-listing helpers for the find tool.
//
// Interpret a single user-supplied target (glob, directory, or file) and
// derive the search root, the effective glob, and whether the scan should
// recurse. Mirrors the semantics of the `@oh-my-pi` reference tool:
//
// - `*.rs` — bare glob: search the tree rooted at the target recursively
//   (`**/*.rs`).
// - `src/*.rs` — glob with an explicit base directory: scoped to `src`,
//   single level (`src/*.rs`, NOT `src/sub/*.rs`).
// - `src/**/*.rs` — already-recursive glob: unchanged.
// - `src` — directory literal: list everything under it recursively.
// - `foo.rs` — file literal (no glob chars and a file exists): the path
//   itself is returned as the single match.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cosh_sdk::find::{FileType, GlobMatch, GlobOptions, glob as sdk_glob};

use super::types::{Glob, GlobEntry, GlobOutput};

/// Detect glob-syntax characters in a string.
#[must_use]
pub fn has_glob_path_chars(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'))
}

/// Split a semicolon-delimited list of paths into entries, trimming empties.
///
/// `toPathList(null)` yields an empty list; callers default to `["."]`.
#[must_use]
pub fn to_path_list(input: Option<&str>) -> Vec<String> {
    input
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A single path input parsed into a search root + effective glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFindPattern {
    /// Directory to walk (`"."` when the input was a bare glob).
    pub base_path: PathBuf,
    /// Glob pattern to match against paths relative to `base_path`.
    pub glob_pattern: String,
    /// Whether the input itself contained glob characters.
    pub has_glob: bool,
    /// Whether the scan should recurse (derived from the pattern shape, not a
    /// user flag): bare globs and `**/` patterns recur; `dir/*` does not.
    pub recursive: bool,
}

/// Parse a single path input into its recursion-aware search root and glob.
///
/// A bare glob (`*.ts`) gets `**/` prepended (recursive). A glob that already
/// carries a base directory (`src/*.ts`) stays scoped to that directory
/// (shallow): the walker must not silently recurse into `src/sub/nested.ts`.
/// A directory literal (`src`) lists everything under it; a file literal
/// (`foo.rs`) also sets `has_glob: false` so the caller can return the file
/// directly.
#[must_use]
pub fn parse_find_pattern(input: &str) -> ParsedFindPattern {
    let normalized = input.replace('\\', "/");
    let normalized = normalized.trim().trim_end_matches('/');

    match normalized
        .char_indices()
        .find(|(_, c)| matches!(c, '*' | '?' | '[' | '{'))
    {
        Some((idx, _)) => {
            let base = normalized[..idx].trim_end_matches('/');
            let glob_part = &normalized[idx..];
            let base_path = if base.is_empty() {
                PathBuf::from(".")
            } else {
                PathBuf::from(base)
            };
            // A bare glob (no base dir) recurses unless it already starts with
            // `**`. A glob with an explicit base dir stays shallow — UNLESS the
            // glob itself is already recursive (`src/**/*.rs`).
            let recursive = base.is_empty() || glob_part.starts_with("**");
            let glob_pattern = if recursive && !glob_part.starts_with("**") {
                format!("**/{glob_part}")
            } else {
                glob_part.to_string()
            };
            ParsedFindPattern {
                base_path,
                glob_pattern,
                has_glob: true,
                recursive,
            }
        }
        None => ParsedFindPattern {
            base_path: PathBuf::from(normalized),
            glob_pattern: String::new(),
            has_glob: false,
            recursive: false,
        },
    }
}

/// Resolve a possibly-relative path fragment against the CWD.
#[must_use]
pub fn resolve_to_cwd(part: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(part);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Format `path` relative to `cwd` when it lives under it, keeping the path
/// absolute otherwise (so externally-scoped matches stay unambiguous).
///
/// When `trailing_slash` is set, directories are emitted with a trailing `/`
/// (the convention the TUI renderer uses to distinguish files from dirs).
#[must_use]
pub fn format_path_relative_to_cwd(path: &str, cwd: &Path, trailing_slash: bool) -> String {
    let mut out = resolve_to_cwd(path, cwd)
        .strip_prefix(cwd)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string().replace('\\', "/"));
    if trailing_slash && !out.ends_with('/') {
        out.push('/');
    }
    out
}

// Output formatting for glob results: `flat`, `grouped`, and `tree`.
//
// These shape how the matched paths are presented to the model (and re-used
// by the TUI for its expanded view). All formats produce stable, sorted
// output so the same search always renders identically.

/// Output layout for a glob result set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathFormat {
    /// Every match on its own line, no grouping.
    #[default]
    Flat,
    /// Grouped by parent directory with a header line per directory.
    Grouped,
    /// Indented tree by path depth.
    Tree,
}

impl PathFormat {
    /// Parse a user-supplied format string. Accepts the aliases the schema
    /// documents (`"flat"`, `"grouped"`, `"tree"`).
    ///
    /// # Errors
    /// Returns an error for any other value so the schema stays honest about
    /// supported formats.
    pub fn parse_format(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "flat" => Ok(Self::Flat),
            "grouped" => Ok(Self::Grouped),
            "tree" => Ok(Self::Tree),
            other => Err(format!(
                "invalid format `{other}`; expected \"flat\", \"grouped\", or \"tree\""
            )),
        }
    }
}

/// Render `paths` in the requested format.
///
/// Input paths are assumed to be forward-slash asset-relative (as produced by
/// the glob tool). Grouping uses the parent directory of each path; matches at
/// the root of the search scope use `"."` as the header.
pub fn format_paths(paths: &[String], format: PathFormat) -> String {
    match format {
        PathFormat::Flat => paths.join("\n"),
        PathFormat::Grouped => format_grouped(paths),
        PathFormat::Tree => format_tree(paths),
    }
}

/// Group paths by parent directory, emitting a header line per group.
///
/// Groups are ordered by directory; entries within a group are sorted so the
/// output is deterministic regardless of the (parallel) walk order.
fn format_grouped(paths: &[String]) -> String {
    let mut groups: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for p in paths {
        groups.entry(parent_dir(p).to_string()).or_default().push(p);
    }
    let mut out = Vec::new();
    for (dir, mut entries) in groups {
        out.push(format!("{dir}/"));
        entries.sort_unstable();
        for e in entries {
            let rel = e.strip_prefix(&format!("{dir}/")).unwrap_or(e).to_string();
            out.push(format!("  {rel}"));
        }
    }
    out.join("\n")
}

/// Render a depth-indented tree, printing each directory once at its own
/// depth and collapsing children beneath it.
fn format_tree(paths: &[String]) -> String {
    // Collect a node set: every component prefix of every path, so an inner
    // directory shows up even when only its children matched.
    let mut nodes: BTreeMap<String, u16> = BTreeMap::new();
    for p in paths {
        let comps: Vec<&str> = p.split('/').filter(|c| !c.is_empty()).collect();
        for (depth, _name) in comps.iter().enumerate() {
            let key = comps[..=depth].join("/");
            nodes.entry(key).or_insert(depth as u16);
        }
    }
    if nodes.is_empty() {
        return String::new();
    }
    // A node is a directory when some other node extends it (`a/` -> `a/b`).
    // Iteration is depth-first pre-order: parents always precede children, and
    // siblings are lexicographically sorted.
    let mut out: Vec<String> = Vec::new();
    for (node, depth) in &nodes {
        let indent = "  ".repeat(*depth as usize);
        let name = node.rsplit('/').next().unwrap_or(node);
        let is_dir = nodes
            .keys()
            .any(|other| other != node && other.starts_with(&format!("{node}/")));
        if is_dir {
            out.push(format!("{indent}{name}/"));
        } else {
            out.push(format!("{indent}{name}"));
        }
    }
    out.join("\n")
}

/// Parent directory of a path, using `/` separators. `"file.rs"` -> `"."`.
pub(super) fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) if idx > 0 => &path[..idx],
        Some(_) => ".",
        None => ".",
    }
}

/// Default timeout for a glob search (milliseconds). Prevents a scan over a
/// huge tree from blocking the agent loop indefinitely. On expiry the partial
/// results are returned with `timed_out: true` instead of a blind error.
pub const DEFAULT_GLOB_TIMEOUT_MS: u32 = 5000;

/// Default result cap for `find_glob`, matching the reference tool (oh-my-pi).
pub(crate) const DEFAULT_GLOB_LIMIT: u32 = 200;
/// Hard ceiling for `find_glob` results. The caller's `limit` can only LOWER
/// the cap; a larger value is clamped down to this ceiling, never honored.
pub(crate) const MAX_GLOB_LIMIT: u32 = 200;

/// Callback invoked for every match as the scan finds it. Runs on walker
/// worker threads, so it must be cheap and `Sync`.
pub type GlobMatchCallback = dyn Fn(&GlobMatch) + Send + Sync;

/// A resolved search root plus the effective glob applied within it.
///
/// Built by the tool layer from a single user `path`/`paths` entry, which may
/// itself be a glob, a directory literal, or a file literal. The parser lives
/// in this module; `Find::glob_full` converts raw entries into specs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobTargetSpec {
    /// Absolute resolved directory (or literal file) to search.
    pub base_path: PathBuf,
    /// Glob to match within `base_path`. Empty for a literal file/dir entry
    /// whose whole content is the result (in which case the effective glob is
    /// a full-tree `**` match).
    pub pattern: String,
    /// Whether the user's entry carried glob characters. A `has_glob: false`
    /// base that is an existing file short-circuits the walk: the file itself
    /// is returned as the single entry.
    pub has_glob: bool,
}

/// Find filesystem entries matching a glob pattern.
///
/// Searches `path` for entries matching `pattern`. Results can be
/// filtered by filesystem kind, sorted by modification time, and bounded by a
/// result count or a timeout.
///
/// # Errors
/// Returns an error when the search path does not exist or is not a directory,
/// the glob pattern is invalid, an unknown `file_type` string is given, or the
/// operation is cancelled by a timeout before any work could be salvaged.
pub fn glob(glob: &Glob, pattern: &str, path: &str) -> Result<GlobOutput, String> {
    glob_with(glob, pattern, &[path.to_string()], None)
}

/// Like [`glob`], but searches one or more `targets` in a single call.
///
/// Each target is walked as its own root (per-path roots keep each scan
/// bounded to exactly what was asked). Results are merged with dedup and,
/// when `sort_by_mtime` is set, re-ranked by mtime so the top-N is correct
/// across all roots. Missing targets are skipped and reported in
/// `missing_paths`; the call only fails when EVERY target is missing.
///
/// `on_match` streams each match as it is found, with paths rebased to the
/// targets' common scope so multi-root output stays unique.
///
/// # Errors
/// Returns an error when every target is missing, a target is not a
/// directory, the glob pattern is invalid, an unknown `file_type` string is
/// given, or the operation is cancelled by a timeout before any work could be
/// salvaged.
pub fn glob_with(
    glob: &Glob,
    pattern: &str,
    targets: &[String],
    on_match: Option<Arc<GlobMatchCallback>>,
) -> Result<GlobOutput, String> {
    // Normalize each pattern through the same parser the tool uses so the
    // SDK `recursive: false` behavior is consistent: bare globs become
    // `**/…`, scoped globs are folded into the target's base path.
    let parsed = parse_find_pattern(pattern);
    let specs: Vec<GlobTargetSpec> = targets
        .iter()
        .map(|t| {
            let base = PathBuf::from(t);
            let base = if parsed.base_path == Path::new(".") {
                base
            } else {
                base.join(&parsed.base_path)
            };
            GlobTargetSpec {
                base_path: base,
                pattern: parsed.glob_pattern.clone(),
                has_glob: parsed.has_glob,
            }
        })
        .collect();
    glob_targets_with(glob, &specs, on_match, None)
}

/// Search from per-target [`GlobTargetSpec`]s.
///
/// `cwd`, when provided, rebases output paths to be CWD-relative (feature:
/// consistent, predictable paths) and records the working directory on the
/// output so the TUI can build absolute OSC 8 file hyperlinks. When it is
/// `None`, paths are rebased to the targets' common ancestor (the legacy
/// behaviour of [`glob_with`]).
pub fn glob_targets_with(
    glob: &Glob,
    specs: &[GlobTargetSpec],
    on_match: Option<Arc<GlobMatchCallback>>,
    cwd: Option<&Path>,
) -> Result<GlobOutput, String> {
    if specs.is_empty() {
        return Err("no search targets provided".to_string());
    }
    let file_type = glob.file_type.as_deref().map(parse_file_type).transpose()?;
    let format = glob
        .format
        .as_deref()
        .map(PathFormat::parse_format)
        .transpose()?
        .unwrap_or_default();
    // A sane default timeout: on expiry the partials are returned with
    // `timed_out` instead of a blind error.
    let timeout_ms = glob.timeout_ms.or(Some(DEFAULT_GLOB_TIMEOUT_MS));

    // Tolerate missing targets: skip them, fail only when all are missing.
    let mut missing_paths: Vec<String> = Vec::new();
    let mut valid: Vec<&GlobTargetSpec> = Vec::new();
    for target in specs {
        if target.base_path.exists() {
            valid.push(target);
        } else {
            missing_paths.push(target.base_path.to_string_lossy().to_string());
        }
    }
    if valid.is_empty() {
        return Err(format!("Path not found: {}", missing_paths.join(", ")));
    }

    let abs_roots: Vec<PathBuf> = valid.iter().map(|t| t.base_path.clone()).collect();
    let ancestor = if valid.len() > 1 {
        super::common_ancestor(&abs_roots)
    } else {
        None
    };
    // Scope label: CWD-relative when a CWD is available (feature: predictable
    // relative paths), otherwise the common ancestor or the single target.
    let scope_buf = ancestor
        .clone()
        .unwrap_or_else(|| valid[0].base_path.clone());
    let scope = cwd
        .map(|cwd| format_path_relative_to_cwd(&scope_buf.to_string_lossy(), cwd, true))
        .unwrap_or_else(|| scope_buf.to_string_lossy().replace('\\', "/"));

    // Fetch one extra result per target so a cap hit is detectable: getting
    // more than `max_results` proves the cap cut the list.
    let fetch_max = glob.max_results.map(|m| m.saturating_add(1));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged: Vec<GlobEntry> = Vec::new();
    let mut timed_out = false;
    let mut limit_reached = false;

    for target in valid {
        let base_str = target.base_path.to_string_lossy().to_string();

        // A literal file target (no glob chars AND an existing file)
        // short-circuits the walk: the file is returned directly.
        if !target.has_glob && target.base_path.is_file() {
            make_literal_file_entry(
                &target.base_path,
                cwd,
                ancestor.as_deref(),
                &mut seen,
                &mut merged,
            );
            continue;
        }

        let wrapped: Option<Arc<GlobMatchCallback>> = on_match.as_ref().map(|cb| {
            let cb = cb.clone();
            let base_owned = base_str.clone();
            let ancestor = ancestor.clone();
            let cb_arc: Arc<GlobMatchCallback> = Arc::new(move |m: &GlobMatch| {
                let rel = rebase_path(&base_owned, &m.path, ancestor.as_deref());
                cb(&GlobMatch {
                    path: rel,
                    file_type: m.file_type,
                    mtime: m.mtime,
                    size: m.size,
                });
            });
            cb_arc
        });

        let sdk_result = sdk_glob(GlobOptions {
            pattern: if target.pattern.is_empty() {
                "**".to_string()
            } else {
                target.pattern.clone()
            },
            path: base_str.clone(),
            file_type,
            // Recursion is already encoded in the pattern by the tool's
            // parser: bare globs arrive as `**/…` (recursive), scoped globs as
            // `dir/…` (shallow). Disable the SDK's own `**/`-prepending so a
            // scoped `*.rs` does not silently escalate into `**/*.rs`.
            recursive: Some(false),
            hidden: glob.hidden,
            max_results: fetch_max,
            gitignore: glob.gitignore,
            sort_by_mtime: glob.sort_by_mtime,
            cache: None,
            include_node_modules: None,
            timeout_ms,
            on_match: wrapped,
        })?;

        timed_out = timed_out || sdk_result.timed_out;
        let entry_count = sdk_result.matches.len() as u32;
        for m in sdk_result.matches {
            let abs = Path::new(&base_str).join(&m.path);
            let display = if cwd.is_some() || ancestor.is_some() {
                display_path(&abs, cwd, ancestor.as_deref(), m.file_type == FileType::Dir)
            } else {
                // Legacy single-target calls (no CWD) keep paths relative to
                // the searched root.
                with_trailing_slash(&m.path.replace('\\', "/"), m.file_type == FileType::Dir)
            };
            if seen.insert(display.clone()) {
                merged.push(GlobEntry {
                    path: display,
                    file_type: file_type_str(m.file_type).to_owned(),
                    mtime_ms: m.mtime,
                    size_bytes: m.size,
                });
            }
        }
        // A capped target means more entries may exist than were returned.
        if let Some(max) = glob.max_results
            && entry_count > max
        {
            limit_reached = true;
        }
    }

    // Global re-rank when sorting: each target's results were individually
    // capped, so the merged top-N must reflect the global mtime order.
    if glob.sort_by_mtime.unwrap_or(false) {
        merged.sort_by(|a, b| {
            b.mtime_ms
                .unwrap_or(0.0)
                .total_cmp(&a.mtime_ms.unwrap_or(0.0))
                .then_with(|| a.path.cmp(&b.path))
        });
    }
    if let Some(max) = glob.max_results {
        if merged.len() as u32 > max {
            limit_reached = true;
        }
        merged.truncate(max as usize);
    }

    let total = u32::try_from(merged.len().min(u32::MAX as usize)).unwrap_or(u32::MAX);

    let (useless, mut note) = if merged.is_empty() && !timed_out {
        (
            Some(true),
            Some("No files found matching pattern".to_string()),
        )
    } else if timed_out && merged.is_empty() {
        (
            None,
            Some(
                "Glob timed out before finding any matches — the scan is incomplete, NOT proof of absence. The walk is bounded by directory size, not pattern width; scope the search to a deeper directory (e.g. `sub/dir/*.ext` instead of `*.ext` at a huge root)."
                    .to_string(),
            ),
        )
    } else if timed_out {
        (
            None,
            Some(
                "Glob timed out; results are partial and incomplete — scope to a deeper directory instead of retrying blindly"
                    .to_string(),
            ),
        )
    } else if limit_reached {
        (
            None,
            Some(
                "Limit reached — more entries may exist; narrow the pattern or add file_type to reduce results"
                    .to_string(),
            ),
        )
    } else {
        (None, None)
    };

    if !missing_paths.is_empty() {
        let missing_note = format!("Skipped missing paths: {}", missing_paths.join(", "));
        note = Some(match note {
            Some(n) => format!("{n}\n{missing_note}"),
            None => missing_note,
        });
    }

    let path_list: Vec<String> = merged.iter().map(|m| m.path.clone()).collect();
    let formatted = if path_list.is_empty() {
        String::new()
    } else {
        format_paths(&path_list, format)
    };

    Ok(GlobOutput {
        matches: merged,
        total,
        limit_reached: if limit_reached { Some(true) } else { None },
        timed_out: if timed_out { Some(true) } else { None },
        note,
        useless,
        missing_paths: if missing_paths.is_empty() {
            None
        } else {
            Some(missing_paths)
        },
        formatted,
        scope,
        cwd: cwd.map(|p| p.to_string_lossy().to_string()),
    })
}

/// Append a literal-file entry for `path` and dedupe against `seen`.
fn make_literal_file_entry(
    path: &PathBuf,
    cwd: Option<&Path>,
    ancestor: Option<&Path>,
    seen: &mut std::collections::HashSet<String>,
    merged: &mut Vec<GlobEntry>,
) {
    let display = display_path(path, cwd, ancestor, false);
    if seen.insert(display.clone()) {
        let meta = std::fs::metadata(path).ok();
        merged.push(GlobEntry {
            path: display,
            file_type: "file".to_string(),
            mtime_ms: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64() * 1000.0),
            size_bytes: meta.map(|m| m.len() as f64),
        });
    }
}

/// Rebase a target-relative path onto `ancestor` or return it unchanged.
fn rebase_path(target: &str, relative: &str, ancestor: Option<&Path>) -> String {
    let Some(ancestor) = ancestor else {
        return relative.to_string();
    };
    let abs = Path::new(target).join(relative);
    abs.strip_prefix(ancestor)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| relative.to_string())
}

/// Render a match path: CWD-relative when `cwd` is set and the path lives
/// under it, else rebased to the common `ancestor`, else the raw path.
fn display_path(
    abs: &Path,
    cwd: Option<&Path>,
    ancestor: Option<&Path>,
    trailing_slash: bool,
) -> String {
    if let Some(cwd) = cwd
        && let Ok(rel) = abs.strip_prefix(cwd)
    {
        return with_trailing_slash(&rel.to_string_lossy().replace('\\', "/"), trailing_slash);
    }
    if let Some(ancestor) = ancestor
        && let Ok(rel) = abs.strip_prefix(ancestor)
    {
        return with_trailing_slash(&rel.to_string_lossy().replace('\\', "/"), trailing_slash);
    }
    with_trailing_slash(&abs.to_string_lossy().replace('\\', "/"), trailing_slash)
}

fn with_trailing_slash(s: &str, trailing_slash: bool) -> String {
    let mut s = s.to_owned();
    if trailing_slash && !s.ends_with('/') {
        s.push('/');
    }
    s
}

#[must_use]
const fn file_type_str(ft: FileType) -> &'static str {
    match ft {
        FileType::File => "file",
        FileType::Dir => "dir",
        FileType::Symlink => "symlink",
    }
}

fn parse_file_type(s: &str) -> Result<FileType, String> {
    match s {
        "file" => Ok(FileType::File),
        "dir" => Ok(FileType::Dir),
        "symlink" => Ok(FileType::Symlink),
        other => Err(format!(
            "invalid file_type `{other}`; expected \"file\", \"dir\", or \"symlink\""
        )),
    }
}
