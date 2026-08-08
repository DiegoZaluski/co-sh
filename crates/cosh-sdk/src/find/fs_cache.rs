//! Shared filesystem scan cache for discovery tools (glob, fd).
//!
//! Provides a TTL-based cache of scanned directory entries, with:
//! - Global policy (no per-call TTL tuning)
//! - Explicit invalidation for agent file mutations
//! - Empty-result fast recheck to avoid stale negatives
//!
//! # Policy Configuration (environment overrides)
//! - `FS_SCAN_CACHE_TTL_MS`       – default `1000`
//! - `FS_SCAN_EMPTY_RECHECK_MS`   – default `200`
//! - `FS_SCAN_CACHE_MAX_ENTRIES`   – default `16`
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use ignore::{ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};

use crate::find::task;

// Public types (re-exported by glob for backward compatibility)

/// Resolved filesystem entry kind for glob filters and match metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    /// Regular file.
    File = 1,
    /// Directory.
    Dir = 2,
    /// Symbolic link.
    Symlink = 3,
}

/// A single filesystem entry from a directory scan.
#[derive(Clone)]
pub struct GlobMatch {
    /// Relative path from the search root, using forward slashes.
    pub path: String,
    /// Resolved filesystem type for the match.
    pub file_type: FileType,
    /// Modification time in milliseconds since Unix epoch (from
    /// `symlink_metadata`).
    pub mtime: Option<f64>,
    /// File size in bytes for regular files.
    pub size: Option<f64>,
}

// Cache policy

#[must_use]
pub fn cache_ttl_ms() -> u64 {
    static CACHE_TTL_MS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        std::env::var("FS_SCAN_CACHE_TTL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_000)
    });
    *CACHE_TTL_MS
}

#[must_use]
pub fn empty_recheck_ms() -> u64 {
    static EMPTY_RECHECK_MS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        std::env::var("FS_SCAN_EMPTY_RECHECK_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200)
    });
    *EMPTY_RECHECK_MS
}

#[must_use]
pub fn max_cache_entries() -> usize {
    static MAX_CACHE_ENTRIES: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        std::env::var("FS_SCAN_CACHE_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16)
    });
    *MAX_CACHE_ENTRIES
}

#[must_use]
pub fn grep_workers() -> usize {
    static GREP_WORKERS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        std::env::var("PI_GREP_WORKERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4)
    });
    *GREP_WORKERS
}

// Cache internals

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    root: PathBuf,
    include_hidden: bool,
    use_gitignore: bool,
    skip_node_modules: bool,
    detail: ScanDetail,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScanDetail {
    Minimal,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "maps directly to user-facing options"
)]
pub struct ScanOptions {
    pub include_hidden: bool,
    pub use_gitignore: bool,
    pub skip_node_modules: bool,
    pub follow_links: bool,
    pub detail: ScanDetail,
}

#[derive(Clone)]
struct CacheEntry {
    created_at: Instant,
    entries: Vec<GlobMatch>,
}

static FS_CACHE: LazyLock<Mutex<HashMap<CacheKey, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Result of a cache-aware scan, including the age of the cached data.
pub struct ScanResult {
    /// Scanned filesystem entries.
    pub entries: Vec<GlobMatch>,
    /// How old the cached data is in milliseconds (0 = freshly scanned).
    pub cache_age_ms: u64,
    /// `true` when the scan was cut short by the timeout and `entries` only
    /// holds the partial results collected up to that point. The scan is
    /// INCOMPLETE — an empty `entries` with `timed_out: true` is not proof of
    /// absence.
    pub timed_out: bool,
}

#[allow(clippy::expect_used)]
fn evict_oldest() {
    let mut cache = FS_CACHE.lock().expect("FS_CACHE lock poisoned");
    if cache.len() > max_cache_entries()
        && let Some(oldest_key) = cache
            .iter()
            .min_by_key(|entry| entry.1.created_at)
            .map(|entry| entry.0.clone())
    {
        cache.remove(&oldest_key);
    }
}

// Path utilities

/// Resolve a search path string to a canonical `PathBuf` (must be a directory).
///
/// # Errors
/// Returns an error if the path cannot be resolved, the current directory
/// cannot be determined, or the resolved path is not a directory.
pub fn resolve_search_path(path: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    let root = if candidate.is_absolute() {
        candidate
    } else {
        let cwd = std::env::current_dir().map_err(|err| format!("Failed to resolve cwd: {err}"))?;
        cwd.join(candidate)
    };
    let metadata = std::fs::metadata(&root).map_err(|err| format!("Path not found: {err}"))?;
    if !metadata.is_dir() {
        return Err("Search path must be a directory".to_string());
    }
    Ok(std::fs::canonicalize(&root).unwrap_or(root))
}

/// Normalize a filesystem path to a forward-slash relative string.
#[must_use]
pub fn normalize_relative_path<'a>(root: &Path, path: &'a Path) -> Cow<'a, str> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if cfg!(windows) {
        let relative = relative.to_string_lossy();
        if relative.contains('\\') {
            Cow::Owned(relative.replace('\\', "/"))
        } else {
            relative
        }
    } else {
        relative.to_string_lossy()
    }
}

#[must_use]
pub fn contains_component(path: &Path, target: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value == target)
    })
}

#[must_use]
pub fn should_skip_path(path: &Path, mentions_node_modules: bool) -> bool {
    // Always skip VCS internals; they are noise for user-facing discovery.
    if contains_component(path, ".git") {
        return true;
    }
    if !mentions_node_modules && contains_component(path, "node_modules") {
        // Skip node_modules by default unless explicitly requested/pattern-matched.
        return true;
    }
    false
}

fn file_type_from_std(file_type: std::fs::FileType) -> Option<FileType> {
    if file_type.is_symlink() {
        Some(FileType::Symlink)
    } else if file_type.is_dir() {
        Some(FileType::Dir)
    } else if file_type.is_file() {
        Some(FileType::File)
    } else {
        None
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "mtime in millis is within f64 precision"
)]
fn mtime_ms(metadata: &std::fs::Metadata) -> Option<f64> {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
}

#[must_use]
pub fn classify_file_type(path: &Path) -> Option<(FileType, Option<f64>, Option<u64>)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let file_type = file_type_from_std(metadata.file_type())?;
    let size = if file_type == FileType::File {
        Some(metadata.len())
    } else {
        None
    };
    Some((file_type, mtime_ms(&metadata), size))
}

// Walker + collection

/// Builds a deterministic filesystem walker configured for visibility and
/// ignore rules.
///
/// When `skip_node_modules` is true, `node_modules` directories are pruned at
/// traversal time (not just filtered post-scan). `.git` is always skipped.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "matches WalkBuilder option fields"
)]
#[must_use]
pub fn build_walker(
    root: &Path,
    include_hidden: bool,
    use_gitignore: bool,
    skip_node_modules: bool,
    follow_links: bool,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .follow_links(follow_links)
        .sort_by_file_path(Ord::cmp)
        // filter_entry controls whether to yield an entry AND whether to descend
        // into a directory. Returning false for a directory skips the entire subtree.
        .filter_entry(move |entry| {
            let name = entry.file_name().to_str().unwrap_or_default();
            // Always skip .git
            if name == ".git" {
                return false;
            }
            // Skip node_modules when skip_node_modules is true
            if skip_node_modules && name == "node_modules" {
                return false;
            }
            true
        });

    if use_gitignore {
        // Honor repository and global ignore files for repo-like behavior.
        builder
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .ignore(true)
            .parents(true);
    } else {
        // Disable all ignore sources for exhaustive filesystem traversal.
        builder
            .git_ignore(false)
            .git_exclude(false)
            .git_global(false)
            .ignore(false)
            .parents(false);
    }

    builder
}

struct EntryVisitor<'a> {
    root: &'a Path,
    detail: ScanDetail,
    ct: &'a task::CancelToken,
    entries: Vec<GlobMatch>,
    shared_entries: Arc<Mutex<Vec<Vec<GlobMatch>>>>,
    error: Arc<Mutex<Option<task::AbortReason>>>,
    visited: usize,
}

#[allow(clippy::expect_used)]
impl Drop for EntryVisitor<'_> {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let entries = std::mem::take(&mut self.entries);
        self.shared_entries
            .lock()
            .expect("entry collection lock poisoned")
            .push(entries);
    }
}

#[allow(clippy::expect_used)]
impl ParallelVisitor for EntryVisitor<'_> {
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
        if let Some(entry) = collect_entry(self.root, &entry, self.detail) {
            self.entries.push(entry);
        }
        WalkState::Continue
    }
}

struct EntryVisitorBuilder<'a> {
    root: &'a Path,
    detail: ScanDetail,
    ct: &'a task::CancelToken,
    shared_entries: Arc<Mutex<Vec<Vec<GlobMatch>>>>,
    error: Arc<Mutex<Option<task::AbortReason>>>,
}

impl<'a> ParallelVisitorBuilder<'a> for EntryVisitorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
        Box::new(EntryVisitor {
            root: self.root,
            detail: self.detail,
            ct: self.ct,
            entries: Vec::new(),
            shared_entries: Arc::clone(&self.shared_entries),
            error: Arc::clone(&self.error),
            visited: 0,
        })
    }
}

/// Scans filesystem entries and records normalized relative paths with file
/// metadata.
///
/// Returns a `(entries, timed_out)` pair: on a mid-scan timeout the entries
/// collected so far are kept with `timed_out: true` instead of failing the
/// whole scan — an empty partial is an incomplete scan, not proof of absence.
/// A timeout that elapses BEFORE the walk starts still surfaces as an error
/// (nothing was collected, so there is nothing to salvage).
#[allow(clippy::expect_used)]
fn collect_entries(
    root: &Path,
    options: ScanOptions,
    ct: &task::CancelToken,
) -> Result<(Vec<GlobMatch>, bool), String> {
    let mut builder = build_walker(
        root,
        options.include_hidden,
        options.use_gitignore,
        options.skip_node_modules,
        options.follow_links,
    );
    let workers = grep_workers();
    if workers > 0 {
        builder.threads(workers);
    }
    let shared_entries = Arc::new(Mutex::new(Vec::new()));
    let error = Arc::new(Mutex::new(None));
    let mut visitor_builder = EntryVisitorBuilder {
        root,
        detail: options.detail,
        ct,
        shared_entries: Arc::clone(&shared_entries),
        error: Arc::clone(&error),
    };
    ct.heartbeat()?;
    builder.build_parallel().visit(&mut visitor_builder);

    let walk_error = error.lock().expect("error lock poisoned").take();
    let entries: Vec<GlobMatch> = shared_entries
        .lock()
        .expect("entry collection lock poisoned")
        .drain(..)
        .flatten()
        .collect();
    if let Some(reason) = walk_error {
        if reason != task::AbortReason::Timeout {
            return Err(reason.to_string());
        }
        // Mid-scan timeout: the per-thread buckets were drained by `Drop` on
        // teardown, so `entries` holds whatever the walk finished before the
        // deadline. Keep it as a partial instead of losing the work.
        return Ok((entries, true));
    }
    Ok((entries, false))
}

pub(crate) fn collect_entry(
    root: &Path,
    entry: &ignore::DirEntry,
    detail: ScanDetail,
) -> Option<GlobMatch> {
    let path = entry.path();
    let relative = normalize_relative_path(root, path);
    if relative.is_empty() {
        // Ignore the synthetic root entry ("" relative path).
        return None;
    }

    let (file_type, mtime, size) = match detail {
        ScanDetail::Minimal => {
            let file_type = file_type_from_std(entry.file_type()?)?;
            (file_type, None, None)
        }
        ScanDetail::Full => {
            let metadata = entry
                .metadata()
                .or_else(|_| std::fs::symlink_metadata(path))
                .ok()?;
            let file_type = file_type_from_std(metadata.file_type())?;
            #[allow(
                clippy::cast_precision_loss,
                reason = "file sizes in typical searches fit in f64"
            )]
            let size = if file_type == FileType::File {
                Some(metadata.len() as f64)
            } else {
                None
            };
            (file_type, mtime_ms(&metadata), size)
        }
    };

    Some(GlobMatch {
        path: relative.into_owned(),
        file_type,
        mtime,
        size,
    })
}

// Cache API

/// Returns scanned entries using the global TTL cache policy.
///
/// The returned [`ScanResult::cache_age_ms`] lets callers implement
/// empty-result fast recheck: if a query produces zero matches and the cache is
/// older than [`empty_recheck_ms()`], call [`force_rescan`] before returning
/// empty.
///
/// # Errors
/// Returns an error if the directory scan fails or the cancel token is triggered.
///
/// # Panics
/// Panics if the internal `FS_CACHE` mutex is poisoned.
#[allow(clippy::expect_used)]
pub fn get_or_scan(
    root: &Path,
    options: ScanOptions,
    ct: &task::CancelToken,
) -> Result<ScanResult, String> {
    let ttl = cache_ttl_ms();
    if ttl == 0 {
        // Caching disabled – always scan fresh.
        let (entries, timed_out) = collect_entries(root, options, ct)?;
        return Ok(ScanResult {
            entries,
            cache_age_ms: 0,
            timed_out,
        });
    }

    let key = CacheKey {
        root: root.to_path_buf(),
        include_hidden: options.include_hidden,
        use_gitignore: options.use_gitignore,
        skip_node_modules: options.skip_node_modules,
        detail: options.detail,
    };

    let now = Instant::now();
    {
        let cache = FS_CACHE.lock().expect("FS_CACHE lock poisoned");
        if let Some(entry) = cache.get(&key) {
            let age = now.duration_since(entry.created_at);
            if age < Duration::from_millis(ttl) {
                return Ok(ScanResult {
                    entries: entry.entries.clone(),
                    cache_age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
                    timed_out: false,
                });
            }
        }
    }
    FS_CACHE
        .lock()
        .expect("FS_CACHE lock poisoned")
        .remove(&key);

    let (entries, timed_out) = collect_entries(root, options, ct)?;
    if !timed_out {
        // Never cache a partial (timed-out) snapshot: it would be served as a
        // seemingly complete scan within the TTL.
        FS_CACHE.lock().expect("FS_CACHE lock poisoned").insert(
            key,
            CacheEntry {
                created_at: now,
                entries: entries.clone(),
            },
        );
        evict_oldest();
    }
    Ok(ScanResult {
        entries,
        cache_age_ms: 0,
        timed_out,
    })
}

/// Force a fresh scan, replacing any existing cache entry.
///
/// Use when a cached query produced zero matches and the cache was old enough
/// to warrant a recheck. When `store` is false, the fresh scan result is
/// returned without repopulating the cache.
///
/// # Errors
/// Returns an error if the directory scan fails or the cancel token is triggered.
///
/// # Panics
/// Panics if the internal `FS_CACHE` mutex is poisoned.
#[allow(clippy::expect_used)]
pub fn force_rescan(
    root: &Path,
    options: ScanOptions,
    store: bool,
    ct: &task::CancelToken,
) -> Result<(Vec<GlobMatch>, bool), String> {
    let key = CacheKey {
        root: root.to_path_buf(),
        include_hidden: options.include_hidden,
        use_gitignore: options.use_gitignore,
        skip_node_modules: options.skip_node_modules,
        detail: options.detail,
    };
    FS_CACHE
        .lock()
        .expect("FS_CACHE lock poisoned")
        .remove(&key);

    let (entries, timed_out) = collect_entries(root, options, ct)?;
    if store && !timed_out {
        // Never cache a partial (timed-out) snapshot: it would be served as a
        // seemingly complete scan within the TTL.
        let now = Instant::now();
        FS_CACHE.lock().expect("FS_CACHE lock poisoned").insert(
            key,
            CacheEntry {
                created_at: now,
                entries: entries.clone(),
            },
        );
        evict_oldest();
    }
    Ok((entries, timed_out))
}

// Invalidation

/// Invalidate cache entries whose root contains `target`.
///
/// Removes any cache entry whose root is a prefix of (or equal to) `target`,
/// because a file mutation under that root makes the scan stale.
///
/// # Panics
/// Panics if the internal `FS_CACHE` mutex is poisoned.
#[allow(clippy::expect_used)]
pub fn invalidate_path(target: &Path) {
    let mut cache = FS_CACHE.lock().expect("FS_CACHE lock poisoned");
    let keys_to_remove: Vec<CacheKey> = cache
        .keys()
        .filter(|key| target.starts_with(&key.root))
        .cloned()
        .collect();
    for key in keys_to_remove {
        cache.remove(&key);
    }
}

/// Clear the entire scan cache.
///
/// # Panics
/// Panics if the internal `FS_CACHE` mutex is poisoned.
#[allow(clippy::expect_used)]
pub fn invalidate_all() {
    FS_CACHE.lock().expect("FS_CACHE lock poisoned").clear();
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use crate::find::task::CancelToken;

    use super::classify_file_type;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time is after UNIX_EPOCH")
                .as_nanos();
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("pi-fs-cache-test-{timestamp}-{counter}"));
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

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        let fifo_path =
            CString::new(path.as_os_str().as_bytes()).expect("fifo path has no NUL bytes");
        // SAFETY: `fifo_path` is a valid CString (NUL-terminated, no interior NULs),
        // so `as_ptr()` yields a valid C string pointer. `0o600` is a valid mode.
        // The CString is alive for the duration of the call.
        let rc = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "create fifo: {}", std::io::Error::last_os_error());
    }

    #[cfg(unix)]
    #[test]
    fn classify_file_type_skips_fifo() {
        let root = TempDirGuard::new();
        let fifo = root.path().join("skip-me.fifo");
        make_fifo(&fifo);

        assert_eq!(classify_file_type(&fifo), None);
    }

    #[test]
    fn build_walker_skips_git_and_node_modules() {
        let root = TempDirGuard::new();
        fs::create_dir_all(root.path().join(".git/objects")).unwrap();
        fs::write(root.path().join(".git/objects/a.txt"), "git obj").unwrap();
        fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();
        fs::write(root.path().join("node_modules/pkg/index.js"), "nm").unwrap();
        fs::write(root.path().join("real.txt"), "ok").unwrap();

        // skip_node_modules: true -> should only see real.txt
        let walker = super::build_walker(root.path(), true, false, true, false);
        let paths: Vec<String> = walker
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.path() != root.path())
            .map(|e| {
                e.path()
                    .strip_prefix(root.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("node_modules") || p.contains(".git")),
            "expected no .git or node_modules entries, got: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "real.txt"),
            "expected real.txt, got: {paths:?}"
        );

        // skip_node_modules: false -> should see node_modules but not .git
        let walker = super::build_walker(root.path(), true, false, false, false);
        let paths: Vec<String> = walker
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.path() != root.path())
            .map(|e| {
                e.path()
                    .strip_prefix(root.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !paths.iter().any(|p| p.contains(".git")),
            "expected no .git entries, got: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("node_modules")),
            "expected node_modules entries, got: {paths:?}"
        );
    }

    #[test]
    fn collect_entries_skips_node_modules() {
        let root = TempDirGuard::new();
        fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();
        fs::write(root.path().join("node_modules/pkg/index.js"), "nm").unwrap();
        fs::write(root.path().join("real.txt"), "ok").unwrap();

        let ct = CancelToken::default();
        let (entries, timed_out) = super::collect_entries(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: true,
                follow_links: false,
                detail: super::ScanDetail::Full,
            },
            &ct,
        )
        .unwrap();
        assert!(!timed_out, "complete scans must not be marked timed_out");
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.contains("node_modules")),
            "expected no node_modules entries, got: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == &"real.txt"),
            "expected real.txt, got: {paths:?}"
        );
    }

    #[test]
    fn collect_entries_respects_pre_cancelled_token() {
        let root = TempDirGuard::new();
        fs::write(root.path().join("real.txt"), "ok").unwrap();

        let ct = CancelToken::new(Some(0));
        std::thread::sleep(Duration::from_millis(1));
        let result = super::collect_entries(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: true,
                follow_links: false,
                detail: super::ScanDetail::Minimal,
            },
            &ct,
        );

        let Err(err) = result else {
            panic!("pre-cancelled scans should fail before returning entries");
        };
        assert!(
            err.to_string().contains("Timeout"),
            "expected timeout cancellation error, got: {err}"
        );
    }

    #[test]
    fn force_rescan_respects_skip_node_modules() {
        let root = TempDirGuard::new();
        // Create a nested node_modules with many files
        for i in 0..100 {
            let pkg_dir = root.path().join(format!("node_modules/pkg-{i}"));
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(pkg_dir.join("index.js"), "x").unwrap();
        }
        fs::write(root.path().join("app.js"), "ok").unwrap();

        let ct = CancelToken::default();

        // With skip: should only get app.js
        let (entries, timed_out) = super::force_rescan(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: true,
                follow_links: false,
                detail: super::ScanDetail::Full,
            },
            false,
            &ct,
        )
        .unwrap();
        assert!(!timed_out);
        assert_eq!(entries.len(), 1, "skip=true got: {}", entries.len());
        assert_eq!(entries[0].path, "app.js");

        // Without skip: should get app.js + 100 node_modules files + directories
        let (entries, timed_out) = super::force_rescan(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: false,
                follow_links: false,
                detail: super::ScanDetail::Full,
            },
            false,
            &ct,
        )
        .unwrap();
        assert!(!timed_out);
        assert!(entries.len() > 100, "skip=false got: {}", entries.len());
    }

    #[test]
    fn scan_detail_controls_metadata_collection() {
        let root = TempDirGuard::new();
        fs::write(root.path().join("real.txt"), "ok").unwrap();

        let ct = CancelToken::default();
        let (minimal, _) = super::collect_entries(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: true,
                follow_links: false,
                detail: super::ScanDetail::Minimal,
            },
            &ct,
        )
        .unwrap();
        let minimal_file = minimal
            .iter()
            .find(|entry| entry.path == "real.txt")
            .expect("minimal scan includes file");
        assert_eq!(minimal_file.mtime, None);
        assert_eq!(minimal_file.size, None);

        let (full, timed_out) = super::collect_entries(
            root.path(),
            super::ScanOptions {
                include_hidden: true,
                use_gitignore: false,
                skip_node_modules: true,
                follow_links: false,
                detail: super::ScanDetail::Full,
            },
            &ct,
        )
        .unwrap();
        assert!(!timed_out);
        let full_file = full
            .iter()
            .find(|entry| entry.path == "real.txt")
            .expect("full scan includes file");
        assert!(full_file.mtime.is_some(), "full scan should include mtime");
        assert_eq!(full_file.size, Some(2.0));
    }
}
