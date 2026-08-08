//! Filesystem discovery with glob patterns, ignore semantics, and shared scan
//! caching.
//!
//! # Overview
//! Resolves a search root, obtains scanned entries via [`fs_cache`], applies
//! glob matching plus optional file-type filtering.
//!
//! The walker always skips `.git`, and skips `node_modules` unless explicitly
//! requested.
//!
//! # Timeout semantics
//! A timeout raised mid-walk is not an error: the entries collected so far are
//! kept and returned with [`GlobResult::timed_out`] set, so the caller can
//! surface partial results instead of forcing a blind retry. A timeout that
//! elapses BEFORE a fresh scan starts still propagates as an error — nothing
//! was found, so there is nothing to salvage. When the entry list comes from a
//! populated scan cache (no walk ran this call), an expired deadline is still
//! not an error: the cached entries are matched and returned with
//! `timed_out: true`.
//!
//! # Example
//! ```ignore
//! // JS: await native.glob({ pattern: "*.rs", path: "." })
//! ```
use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    path::Path,
    sync::{Arc, Mutex},
};

use globset::GlobSet;
use ignore::{ParallelVisitor, ParallelVisitorBuilder, WalkState};

// Re-export entry types so existing `glob::FileType` / `glob::GlobMatch` paths still work.
pub use super::fs_cache::{FileType, GlobMatch};
use super::{fs_cache, glob_util, task};
use task::AbortReason;

/// Callback invoked for every match as the walk finds it. Runs on walker
/// worker threads, so it must be cheap and `Sync`.
pub type GlobMatchCallback = dyn Fn(&GlobMatch) + Send + Sync;

/// Input options for `glob`, including traversal, filtering, and cancellation.
pub struct GlobOptions {
    /// Glob pattern to match (e.g., "*.ts").
    pub pattern: String,
    /// Directory to search.
    pub path: String,
    /// Filter by file type: "file", "dir", or "symlink". Symlinks are
    /// matched for file/dir filters based on their target type.
    pub file_type: Option<FileType>,
    /// Match simple patterns recursively by default (`*.ts` -> recursive).
    pub recursive: Option<bool>,
    /// Include hidden files (default: false).
    pub hidden: Option<bool>,
    /// Maximum number of results to return.
    pub max_results: Option<u32>,
    /// Respect .gitignore files (default: true).
    pub gitignore: Option<bool>,
    /// Enable shared filesystem scan cache (default: false).
    pub cache: Option<bool>,
    /// Sort results by mtime (most recent first) before applying limit.
    pub sort_by_mtime: Option<bool>,
    /// Include `node_modules` entries when the pattern does not explicitly
    /// mention them.
    pub include_node_modules: Option<bool>,
    /// Timeout in milliseconds for the operation.
    pub timeout_ms: Option<u32>,
    /// Called for every match as it is found (before the mtime rank / limit
    /// is applied). Lets callers stream live results while a scan is running.
    pub on_match: Option<Arc<GlobMatchCallback>>,
}

/// Result payload returned by a glob operation.
pub struct GlobResult {
    /// Matched filesystem entries.
    pub matches: Vec<GlobMatch>,
    /// Number of returned matches (`matches.len()`), clamped to `u32::MAX`.
    pub total_matches: u32,
    /// `true` when the scan was cut short by the timeout and `matches` only
    /// holds the partial results found up to that point. The scan is
    /// INCOMPLETE — an empty `matches` with `timed_out: true` is not proof of
    /// absence.
    pub timed_out: bool,
}

/// Internal runtime config for a single glob execution.
#[allow(
    clippy::struct_excessive_bools,
    reason = "internal config with independent flags"
)]
struct GlobConfig {
    root: std::path::PathBuf,
    pattern: String,
    recursive: bool,
    include_hidden: bool,
    file_type_filter: Option<FileType>,
    max_results: usize,
    use_gitignore: bool,
    mentions_node_modules: bool,
    sort_by_mtime: bool,
    use_cache: bool,
    on_match: Option<Arc<GlobMatchCallback>>,
}

#[derive(Clone)]
struct RankedGlobMatch {
    entry: GlobMatch,
}

impl PartialEq for RankedGlobMatch {
    fn eq(&self, other: &Self) -> bool {
        compare_matches_by_rank(&self.entry, &other.entry) == Ordering::Equal
    }
}

impl Eq for RankedGlobMatch {}

impl PartialOrd for RankedGlobMatch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedGlobMatch {
    fn cmp(&self, other: &Self) -> Ordering {
        if match_is_worse(&self.entry, &other.entry) {
            Ordering::Greater
        } else if match_is_worse(&other.entry, &self.entry) {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }
}

fn match_mtime(entry: &GlobMatch) -> f64 {
    entry.mtime.unwrap_or(0.0)
}

fn compare_matches_by_rank(a: &GlobMatch, b: &GlobMatch) -> Ordering {
    match_mtime(b)
        .total_cmp(&match_mtime(a))
        .then_with(|| a.path.cmp(&b.path))
}

fn match_is_worse(a: &GlobMatch, b: &GlobMatch) -> bool {
    compare_matches_by_rank(a, b) == Ordering::Greater
}

/// Returns `true` when `entry` was admitted into the bounded top-`limit` heap
/// (either filling free space or evicting a worse existing entry).
fn push_bounded_match(
    heap: &mut BinaryHeap<RankedGlobMatch>,
    entry: GlobMatch,
    limit: usize,
) -> bool {
    if heap.len() < limit {
        heap.push(RankedGlobMatch { entry });
        return true;
    }

    let Some(worst) = heap.peek() else {
        return false;
    };
    if match_is_worse(&worst.entry, &entry) {
        heap.pop();
        heap.push(RankedGlobMatch { entry });
        return true;
    }
    false
}

fn resolve_symlink_target_type(root: &Path, relative_path: &str) -> Option<FileType> {
    let target_path = root.join(relative_path);
    let metadata = std::fs::metadata(target_path).ok()?;
    if metadata.is_dir() {
        Some(FileType::Dir)
    } else if metadata.is_file() {
        Some(FileType::File)
    } else {
        None
    }
}

fn apply_file_type_filter(entry: &GlobMatch, config: &GlobConfig) -> Option<FileType> {
    let Some(filter) = config.file_type_filter else {
        return Some(entry.file_type);
    };
    if entry.file_type == filter {
        return Some(entry.file_type);
    }
    if entry.file_type != FileType::Symlink {
        return None;
    }
    match filter {
        FileType::File | FileType::Dir => {
            let resolved = resolve_symlink_target_type(&config.root, &entry.path)?;
            if resolved == filter {
                Some(resolved)
            } else {
                None
            }
        }
        FileType::Symlink => None,
    }
}

/// Filter and collect matching entries from a pre-scanned list.
///
/// The scan feeding this list may itself be partial (a mid-scan timeout kept
/// what was collected). A timeout that trips mid-filter only marks the scan
/// incomplete — the collected entries are STILL matched and returned, so a
/// partial snapshot cannot be inadvertently discarded. Non-timeout aborts
/// keep failing the whole search. A timeout that elapses before the first
/// entry is checked surfaces as a fully-filtered partial with `timed_out`
/// set, never as an error (the caller decides whether an empty partial is
/// meaningful).
fn filter_entries(
    entries: &[GlobMatch],
    glob_set: &GlobSet,
    config: &GlobConfig,
    ct: &task::CancelToken,
) -> Result<(Vec<GlobMatch>, bool), String> {
    let mut matches = Vec::new();
    let mut timed_out = false;
    if config.max_results == 0 {
        return Ok((matches, false));
    }

    for entry in entries {
        if let Err(reason) = ct.heartbeat_reason() {
            if reason == AbortReason::Timeout {
                // Keep matching the collected entries; the timeout only means
                // the walk was cut short, not that this partial is useless.
                timed_out = true;
            } else {
                return Err(reason.to_string());
            }
        }
        if fs_cache::should_skip_path(Path::new(&entry.path), config.mentions_node_modules) {
            // Apply post-scan node_modules policy before glob matching.
            continue;
        }
        if !glob_set.is_match(&entry.path) {
            continue;
        }
        let Some(effective_file_type) = apply_file_type_filter(entry, config) else {
            continue;
        };
        let mut matched_entry = entry.clone();
        matched_entry.file_type = effective_file_type;

        if let Some(on_match) = &config.on_match {
            on_match(&matched_entry);
        }
        matches.push(matched_entry);
        // Only early-break when not sorting; mtime sort requires full candidate set.
        if !config.sort_by_mtime && matches.len() >= config.max_results {
            break;
        }
    }
    Ok((matches, timed_out))
}

struct SortedMatchVisitor<'a> {
    glob_set: &'a GlobSet,
    config: &'a GlobConfig,
    top_matches: BinaryHeap<RankedGlobMatch>,
    shared: Arc<Mutex<Vec<GlobMatch>>>,
    error: Arc<Mutex<Option<AbortReason>>>,
    ct: &'a task::CancelToken,
    visited: usize,
}

#[allow(clippy::expect_used)]
impl Drop for SortedMatchVisitor<'_> {
    fn drop(&mut self) {
        if self.top_matches.is_empty() {
            return;
        }
        let drained = std::mem::take(&mut self.top_matches);
        self.shared
            .lock()
            .expect("glob match collection lock poisoned")
            .extend(drained.into_iter().map(|ranked| ranked.entry));
    }
}

#[allow(clippy::expect_used)]
impl ParallelVisitor for SortedMatchVisitor<'_> {
    fn visit(&mut self, entry: std::result::Result<ignore::DirEntry, ignore::Error>) -> WalkState {
        if self.visited == 0 || self.visited >= 128 {
            self.visited = 0;
            if let Err(reason) = self.ct.heartbeat_reason() {
                *self.error.lock().expect("error lock poisoned") = Some(reason);
                return WalkState::Quit;
            }
        }
        self.visited += 1;

        let Ok(entry) = entry else {
            return WalkState::Continue;
        };
        let Some(mut matched_entry) =
            fs_cache::collect_entry(&self.config.root, &entry, fs_cache::ScanDetail::Full)
        else {
            return WalkState::Continue;
        };
        if fs_cache::should_skip_path(
            Path::new(&matched_entry.path),
            self.config.mentions_node_modules,
        ) {
            return WalkState::Continue;
        }
        if !self.glob_set.is_match(&matched_entry.path) {
            return WalkState::Continue;
        }
        let Some(effective_file_type) = apply_file_type_filter(&matched_entry, self.config) else {
            return WalkState::Continue;
        };
        matched_entry.file_type = effective_file_type;
        if let Some(on_match) = &self.config.on_match {
            on_match(&matched_entry);
        }
        push_bounded_match(
            &mut self.top_matches,
            matched_entry,
            self.config.max_results,
        );
        WalkState::Continue
    }
}

struct SortedMatchVisitorBuilder<'a> {
    glob_set: &'a GlobSet,
    config: &'a GlobConfig,
    shared: Arc<Mutex<Vec<GlobMatch>>>,
    error: Arc<Mutex<Option<AbortReason>>>,
    ct: &'a task::CancelToken,
}

impl<'a> ParallelVisitorBuilder<'a> for SortedMatchVisitorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
        Box::new(SortedMatchVisitor {
            glob_set: self.glob_set,
            config: self.config,
            top_matches: BinaryHeap::with_capacity(self.config.max_results.min(1024)),
            shared: Arc::clone(&self.shared),
            error: Arc::clone(&self.error),
            ct: self.ct,
            visited: 0,
        })
    }
}

/// Walk the tree in parallel, keeping a bounded top-`max_results` heap per
/// worker. The union of per-thread heaps always contains the global top-N;
/// `run_glob` re-sorts and truncates afterwards, so the final ranking is
/// deterministic (mtime desc, path tiebreak) regardless of walk order.
///
/// On a mid-walk timeout the per-thread heaps (drained by `Drop`) are returned
/// as partials with `timed_out: true`; a timeout raised before the walk starts
/// is an error.
#[allow(clippy::expect_used)]
fn collect_sorted_matches_uncached(
    glob_set: &GlobSet,
    config: &GlobConfig,
    ct: &task::CancelToken,
) -> Result<(Vec<GlobMatch>, bool), String> {
    let mut builder = fs_cache::build_walker(
        &config.root,
        config.include_hidden,
        config.use_gitignore,
        !config.mentions_node_modules,
        false,
    );
    let workers = fs_cache::grep_workers();
    if workers > 0 {
        builder.threads(workers);
    }
    let shared = Arc::new(Mutex::new(Vec::new()));
    let error = Arc::new(Mutex::new(None));
    let mut visitor_builder = SortedMatchVisitorBuilder {
        glob_set,
        config,
        shared: Arc::clone(&shared),
        error: Arc::clone(&error),
        ct,
    };
    ct.heartbeat()?;
    builder.build_parallel().visit(&mut visitor_builder);

    let walk_error = error.lock().expect("error lock poisoned").take();
    let mut matches =
        std::mem::take(&mut *shared.lock().expect("glob match collection lock poisoned"));

    if let Some(reason) = walk_error {
        if reason != AbortReason::Timeout {
            return Err(reason.to_string());
        }
        // Mid-walk timeout: keep the partials the visitors drained on drop.
        matches.sort_by(compare_matches_by_rank);
        matches.truncate(config.max_results);
        return Ok((matches, true));
    }

    matches.sort_by(compare_matches_by_rank);
    matches.truncate(config.max_results);
    Ok((matches, false))
}

/// Executes matching/filtering over scanned entries.
fn run_glob(config: &GlobConfig, ct: &task::CancelToken) -> Result<GlobResult, String> {
    let glob_set = glob_util::compile_glob(&config.pattern, config.recursive)?;
    if config.max_results == 0 {
        return Ok(GlobResult {
            matches: Vec::new(),
            total_matches: 0,
            timed_out: false,
        });
    }

    let skip_node_modules = !config.mentions_node_modules;
    let scan_options = fs_cache::ScanOptions {
        include_hidden: config.include_hidden,
        use_gitignore: config.use_gitignore,
        skip_node_modules,
        follow_links: false,
        detail: if config.sort_by_mtime {
            fs_cache::ScanDetail::Full
        } else {
            fs_cache::ScanDetail::Minimal
        },
    };
    let streams_bounded_sorted_partials =
        config.sort_by_mtime && !config.use_cache && config.max_results != usize::MAX;
    let (mut matches, timed_out) = if streams_bounded_sorted_partials {
        collect_sorted_matches_uncached(&glob_set, config, ct)?
    } else if config.use_cache {
        let scan = fs_cache::get_or_scan(&config.root, scan_options, ct)?;
        let scan_timed_out = scan.timed_out;
        let (mut filtered, filter_timed_out) =
            filter_entries(&scan.entries, &glob_set, config, ct)?;
        // Empty-result recheck: if we got zero matches from a cached scan that's
        // old enough, force a rescan and try once more before returning empty.
        // Never recheck when the scan OR the filter already timed out — the
        // result is partial, so a fresh scan would be a whole new timeout.
        if filtered.is_empty()
            && !scan_timed_out
            && !filter_timed_out
            && scan.cache_age_ms >= fs_cache::empty_recheck_ms()
        {
            let (fresh, rescan_timed_out) =
                fs_cache::force_rescan(&config.root, scan_options, true, ct)?;
            let (refiltered, re_flag) = filter_entries(&fresh, &glob_set, config, ct)?;
            filtered = refiltered;
            (filtered, rescan_timed_out || re_flag)
        } else {
            (filtered, scan_timed_out || filter_timed_out)
        }
    } else {
        let (fresh, scan_timed_out) =
            fs_cache::force_rescan(&config.root, scan_options, false, ct)?;
        let (filtered, filter_timed_out) = filter_entries(&fresh, &glob_set, config, ct)?;
        (filtered, scan_timed_out || filter_timed_out)
    };

    if config.sort_by_mtime {
        // Sorting mode: rank by mtime descending, then apply max-results truncation.
        matches.sort_by(compare_matches_by_rank);
        matches.truncate(config.max_results);
    }
    let total_matches = u32::try_from(matches.len().min(u32::MAX as usize)).unwrap_or(u32::MAX);
    Ok(GlobResult {
        matches,
        total_matches,
        timed_out,
    })
}

/// Find filesystem entries matching a glob pattern.
///
/// Resolves the search root, scans entries, applies glob and optional file-type
/// filters.
///
/// If `sortByMtime` is enabled with a finite `maxResults`, uncached scans keep
/// only the current top results while traversing instead of collecting the full
/// tree.
///
/// # Errors
/// Returns an error when the search path cannot be resolved, the path is not a
/// directory, the glob pattern is invalid, or cancellation/timeout is
/// triggered before any work could be salvaged.
pub fn glob(options: GlobOptions) -> Result<GlobResult, String> {
    let GlobOptions {
        pattern,
        path,
        file_type,
        recursive,
        hidden,
        max_results,
        gitignore,
        sort_by_mtime,
        cache,
        include_node_modules,
        timeout_ms,
        on_match,
    } = options;

    let pattern = pattern.trim();
    let pattern = if pattern.is_empty() { "*" } else { pattern };
    let pattern = pattern.to_string();

    let ct = task::CancelToken::new(timeout_ms);

    let config = GlobConfig {
        root: fs_cache::resolve_search_path(&path)?,
        include_hidden: hidden.unwrap_or(false),
        file_type_filter: file_type,
        recursive: recursive.unwrap_or(true),
        max_results: max_results.map_or(usize::MAX, |value| value as usize),
        use_gitignore: gitignore.unwrap_or(true),
        mentions_node_modules: include_node_modules
            .unwrap_or_else(|| pattern.contains("node_modules")),
        sort_by_mtime: sort_by_mtime.unwrap_or(false),
        use_cache: cache.unwrap_or(false),
        pattern,
        on_match,
    };
    run_glob(&config, &ct)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{GlobConfig, filter_entries, resolve_symlink_target_type};
    use crate::find::task;

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time is after UNIX_EPOCH")
                .as_nanos();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("pi-glob-test-{pid}-{nanos}-{seq}"));
            fs::create_dir_all(&path).expect("create temp test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn glob_match(relative: &str) -> crate::find::fs_cache::GlobMatch {
        crate::find::fs_cache::GlobMatch {
            path: relative.to_string(),
            file_type: crate::find::fs_cache::FileType::File,
            mtime: Some(1.0),
            size: Some(1.0),
        }
    }

    fn base_config(root: &Path) -> GlobConfig {
        GlobConfig {
            root: root.to_path_buf(),
            pattern: "*.txt".to_string(),
            recursive: true,
            include_hidden: false,
            file_type_filter: None,
            max_results: usize::MAX,
            use_gitignore: false,
            mentions_node_modules: false,
            sort_by_mtime: false,
            use_cache: false,
            on_match: None,
        }
    }

    /// A timeout is not an error: the entries the scan already collected are
    /// STILL matched and returned with `timed_out: true`, so the partial
    /// snapshot is never discarded. An empty partial is an incomplete scan —
    /// never a hard failure.
    #[test]
    fn filter_entries_timeout_keeps_collected_entries() {
        let root = TempDirGuard::new();
        let entries: Vec<_> = (0..50)
            .map(|i| glob_match(&format!("a{i:02}.txt")))
            .collect();

        let ct = task::CancelToken::new(Some(1));
        std::thread::sleep(Duration::from_millis(2));
        let (matches, timed_out) =
            filter_entries(&entries, &compile_all(), &base_config(root.path()), &ct)
                .expect("filter_entries should not fail on timeout");
        assert!(
            timed_out,
            "an elapsed deadline must surface as timed_out partials"
        );
        assert_eq!(
            matches.len(),
            50,
            "the collected entries must survive the timeout, not be dropped"
        );
    }

    /// A non-timeout cancellation must still surface as an error (nothing is
    /// salvaged for explicit aborts).
    #[test]
    fn filter_entries_explicit_abort_is_error() {
        let root = TempDirGuard::new();
        let entries: Vec<_> = (0..10).map(|i| glob_match(&format!("a{i}.txt"))).collect();
        let ct = task::CancelToken::new(None);
        ct.abort_token().abort(task::AbortReason::User);
        let err = match filter_entries(&entries, &compile_all(), &base_config(root.path()), &ct) {
            Err(err) => err,
            Ok(_) => panic!("explicit abort must remain an error"),
        };
        assert_eq!(err, "User");
    }

    /// The `on_match` callback must fire for every match the filter admits.
    #[test]
    fn filter_entries_invokes_on_match() {
        let root = TempDirGuard::new();
        let entries: Vec<_> = (0..5).map(|i| glob_match(&format!("a{i}.txt"))).collect();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut config = base_config(root.path());
        let cb = {
            let seen = std::sync::Arc::clone(&seen);
            std::sync::Arc::new(move |m: &crate::find::fs_cache::GlobMatch| {
                seen.lock().unwrap().push(m.path.clone());
            })
        };
        config.on_match = Some(cb);
        let (matches, timed_out) = filter_entries(
            &entries,
            &compile_all(),
            &config,
            &task::CancelToken::default(),
        )
        .expect("filter_entries should succeed");
        assert!(!timed_out);
        assert_eq!(matches.len(), 5);
        assert_eq!(seen.lock().unwrap().len(), 5);
    }

    /// Symlinks pointing at a directory resolve to `Dir` for the file-type
    /// filter.
    #[cfg(unix)]
    #[test]
    fn symlink_target_type_resolves_directory() {
        let root = TempDirGuard::new();
        fs::create_dir_all(root.path().join("real_dir")).expect("create dir");
        std::os::unix::fs::symlink(root.path().join("real_dir"), root.path().join("link"))
            .expect("create symlink");
        let ft = resolve_symlink_target_type(root.path(), "link");
        assert_eq!(ft, Some(crate::find::fs_cache::FileType::Dir));
    }

    fn compile_all() -> globset::GlobSet {
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("**/*.txt").expect("valid glob"));
        builder.build().expect("build glob set")
    }
}
