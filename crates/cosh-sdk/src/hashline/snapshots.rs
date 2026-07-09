//! Per-session snapshot store used by recovery and the patcher to
//! bind hashline section tags to the exact file content that minted them.
//!
//! A section tag is a content-derived hash of the *whole file* (see
//! [`compute_file_hash`]). Any read of byte-identical content mints the same
//! tag, so reads of one file state fuse onto one anchor and a follow-up edit
//! anchored at any line validates whenever the live file still hashes to it.
//!
//! Producers (typically `read` / `search` / `write` tools) call
//! [`SnapshotStore::record`] with the full normalized text they observed.
//! The store hashes it, dedups against the per-path history, and returns the
//! tag. Consumers (the patcher) resolve a stale tag back to the recorded full
//! text via [`SnapshotStore::by_hash`] and 3-way-merge the would-be edit onto
//! the live content.
//!
//! The [`SnapshotStore`] trait lets callers plug in whatever storage they like
//! (LRU, persistent `SQLite`, etc.). [`InMemorySnapshotStore`] ships as a
//! sensible default backed by `lru`: a bounded set of paths, each with a
//! short history of full-file versions so in-session edit chains can still
//! recover against the version a stale tag names.
use super::format::compute_file_hash;
use lru::LruCache;
use std::num::NonZeroUsize;

/// One full-file version observed at a point in time. The tag the model sees is
/// [`Snapshot::hash`]; recovery replays edits against [`Snapshot::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Canonical path this version belongs to.
    pub path: String,
    /// Full normalized (LF, no BOM) file text as observed.
    pub text: String,
    /// Content-derived tag for [`Snapshot::text`] (see [`compute_file_hash`]).
    pub hash: String,
    /// Timestamp (ms since epoch) the version was recorded.
    pub recorded_at: u64,
}

/// Storage seam for full-file version snapshots. The patcher calls [`head`](SnapshotStore::head)
///
/// for the latest version of a path and [`by_hash`](SnapshotStore::by_hash) when it needs the
/// specific historical version a section's stale tag names.
pub trait SnapshotStore {
    /// Most-recently recorded version for `path`, or `None` if none.
    fn head(&mut self, path: &str) -> Option<Snapshot>;

    /// All recorded versions for `path`, newest first. Returns an empty vec when unknown.
    fn history(&mut self, path: &str) -> Vec<Snapshot> {
        self.head(path).into_iter().collect()
    }

    /// Recorded version for `path` whose tag equals `hash`, or `None`.
    fn by_hash(&mut self, path: &str, hash: &str) -> Option<Snapshot>;

    /// Record the full normalized text of `path` and return its content tag.
    fn record(&mut self, path: &str, full_text: &str) -> String;

    /// Drop the version history for a single path.
    fn invalidate(&mut self, path: &str);

    /// Drop every version history.
    fn clear(&mut self);
}

const DEFAULT_MAX_PATHS: usize = 30;
const DEFAULT_MAX_VERSIONS_PER_PATH: usize = 4;

#[derive(Debug, Clone, Default)]
pub struct InMemorySnapshotStoreOptions {
    /// Maximum number of distinct paths tracked at once (default 30). LRU eviction.
    pub max_paths: Option<NonZeroUsize>,
    /// Maximum full-file versions retained per path (default 4). Oldest dropped first.
    pub max_versions_per_path: Option<usize>,
}

/// In-memory [`SnapshotStore`] backed by `lru`. Per-path history is a
///
/// short ring of full-file versions (oldest dropped first); per-session path
/// tracking is LRU-bounded so cold paths age out automatically.
///
/// Recording byte-identical content again refreshes recency and reuses the
/// existing tag (read fusion); recording new content prepends a fresh version
/// to the path history.
pub struct InMemorySnapshotStore {
    versions: LruCache<String, Vec<Snapshot>>,
    max_versions_per_path: usize,
}

impl InMemorySnapshotStore {
    /// Creates a new `InMemorySnapshotStore`.
    ///
    /// # Panics
    ///
    /// Panics if `max_paths` is zero.
    #[must_use]
    pub fn new(options: &InMemorySnapshotStoreOptions) -> Self {
        let max_paths = options.max_paths.unwrap_or({
            #[allow(clippy::unwrap_used)]
            NonZeroUsize::new(DEFAULT_MAX_PATHS).unwrap()
        });
        Self {
            versions: LruCache::new(max_paths),
            max_versions_per_path: options
                .max_versions_per_path
                .unwrap_or(DEFAULT_MAX_VERSIONS_PER_PATH),
        }
    }
}

impl SnapshotStore for InMemorySnapshotStore {
    fn head(&mut self, path: &str) -> Option<Snapshot> {
        self.versions.get(path)?.first().cloned()
    }

    fn history(&mut self, path: &str) -> Vec<Snapshot> {
        self.versions.get(path).cloned().unwrap_or_default()
    }

    fn by_hash(&mut self, path: &str, hash: &str) -> Option<Snapshot> {
        self.versions
            .get(path)?
            .iter()
            .find(|v| v.hash == hash)
            .cloned()
    }

    fn record(&mut self, path: &str, full_text: &str) -> String {
        let hash = compute_file_hash(full_text);
        let mut history = self.versions.pop(path).unwrap_or_default();

        if let Some(pos) = history.iter().position(|v| v.hash == hash) {
            let mut existing = history.remove(pos);
            existing.recorded_at = now();
            history.insert(0, existing);
            self.versions.put(path.to_string(), history);
            return hash;
        }

        let snapshot = Snapshot {
            path: path.to_string(),
            text: full_text.to_string(),
            hash: hash.clone(),
            recorded_at: now(),
        };
        history.insert(0, snapshot);
        history.truncate(self.max_versions_per_path);
        self.versions.put(path.to_string(), history);
        hash
    }

    fn invalidate(&mut self, path: &str) {
        self.versions.pop(path);
    }

    fn clear(&mut self) {
        self.versions.clear();
    }
}

// Shared ownership via Arc<Mutex<S>> so both Patcher and Recovery
// can reference the same SnapshotStore without duplicating state.
// A std::sync::Mutex is appropriate here — critical sections are
// short (HashMap/LRU ops) and never held across .await points.
use std::sync::{Arc, Mutex};

impl<S: SnapshotStore> SnapshotStore for Arc<Mutex<S>> {
    #[allow(clippy::unwrap_used)]
    fn head(&mut self, path: &str) -> Option<Snapshot> {
        self.lock().unwrap().head(path)
    }
    #[allow(clippy::unwrap_used)]
    fn history(&mut self, path: &str) -> Vec<Snapshot> {
        self.lock().unwrap().history(path)
    }
    #[allow(clippy::unwrap_used)]
    fn by_hash(&mut self, path: &str, hash: &str) -> Option<Snapshot> {
        self.lock().unwrap().by_hash(path, hash)
    }
    #[allow(clippy::unwrap_used)]
    fn record(&mut self, path: &str, full_text: &str) -> String {
        self.lock().unwrap().record(path, full_text)
    }
    #[allow(clippy::unwrap_used)]
    fn invalidate(&mut self, path: &str) {
        self.lock().unwrap().invalidate(path);
    }
    #[allow(clippy::unwrap_used)]
    fn clear(&mut self) {
        self.lock().unwrap().clear();
    }
}

fn now() -> u64 {
    std::time::UNIX_EPOCH
        .elapsed()
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
