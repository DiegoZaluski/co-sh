use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A generic stale-while-revalidate cache backed by JSON on disk.
///
/// - Returns cached data immediately from in-memory storage.
/// - Tracks which keys are currently being revalidated to prevent redundant
///   background fetches.
/// - Persists the in-memory data to disk as JSON on every update.
///
/// The cache directory is resolved via [`ProjectDirs`] as:
/// `{data_dir}/{cache_subdir}/{filename}` (e.g. `~/.local/share/cosh/cache/model.json`).
///
/// Callers must provide a `cache_subdir` relative to the data dir (e.g. `"cache"`).
/// The data dir already includes the app name (`cosh`), so callers should NOT
/// prefix paths with `"cosh/"`.
///
/// # Type parameters
/// - `K`: Cache key type (must be `Eq + Hash + Clone + Serialize + DeserializeOwned`).
/// - `V`: Cached value type (must be `Clone + Serialize + DeserializeOwned`).
///
/// # Example (models use case)
/// ```ignore
/// let mut cache: StaleCache<String, Vec<ModelEntry>> = StaleCache::new("cache", "model.json");
///
/// if let Some(models) = cache.get("openai") {
///     // Use cached models immediately
/// }
/// if !cache.is_revalidating("openai") {
///     cache.start_revalidation("openai".to_string());
///     // Spawn background fetch...
///     // On completion: cache.finish_revalidation("openai".to_string(), new_models);
/// }
/// ```
pub struct StaleCache<K, V> {
    /// In-memory cache data.
    data: HashMap<K, V>,
    /// Set of keys currently being revalidated in background.
    revalidating: HashSet<K>,
    /// Full path to the JSON file on disk.
    file_path: PathBuf,
}

/// Internal JSON envelope stored on disk.
#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "K: Serialize, V: Serialize",
    deserialize = "K: DeserializeOwned + Hash + Eq, V: DeserializeOwned"
))]
struct CacheEnvelope<K, V> {
    data: HashMap<K, V>,
}

impl<K, V> StaleCache<K, V>
where
    K: Eq + Hash + Clone + Serialize + DeserializeOwned,
    V: Clone + Serialize + DeserializeOwned,
{
    /// Create a new cache backed by the given `filename` inside
    /// `{data_dir}/{cache_subdir}/`.
    ///
    /// Automatically loads any existing data from disk on construction.
    /// Creates the cache directory if it does not exist.
    ///
    /// Callers must supply a `cache_subdir` (e.g. `"cache"`). No vendor
    /// path is hardcoded here — each caller decides where under the OS data
    /// directory to place cache files.
    ///
    /// # Panics
    /// Panics if the `ProjectDirs` cannot be determined (e.g. no $HOME set).
    pub fn new(cache_subdir: &str, filename: &str) -> Self {
        let proj_dirs = ProjectDirs::from("", "", "cosh")
            .expect("could not determine project directories (is $HOME set?)");
        let cache_dir = proj_dirs.data_dir().join(cache_subdir);
        std::fs::create_dir_all(&cache_dir).ok();
        let file_path = cache_dir.join(filename);

        let data = Self::load_from_disk(&file_path).unwrap_or_default();
        let revalidating = HashSet::new();

        Self {
            data,
            revalidating,
            file_path,
        }
    }

    /// Get a reference to the cached value for `key`, if available.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.data.get(key)
    }

    /// Returns `true` if `key` is currently being revalidated.
    pub fn is_revalidating(&self, key: &K) -> bool {
        self.revalidating.contains(key)
    }

    /// Mark `key` as being revalidated.
    ///
    /// Returns `false` if the key was already marked — the caller should
    /// skip spawning another background fetch.
    pub fn start_revalidation(&mut self, key: K) -> bool {
        self.revalidating.insert(key)
    }

    /// Mark revalidation as complete for `key` and store the new `value`.
    ///
    /// The updated cache is persisted to disk immediately.
    pub fn finish_revalidation(&mut self, key: K, value: V) {
        self.revalidating.remove(&key);
        self.data.insert(key, value);
        self.save();
    }

    /// Remove `key` from the cache and persist to disk.
    pub fn invalidate(&mut self, key: &K) {
        self.data.remove(key);
        self.revalidating.remove(key);
        self.save();
    }

    /// Clear all revalidation flags (no persistence effect).
    /// Should be called after a batch fetch completes, to clear flags for
    /// providers that may have failed — their old cached data is preserved.
    pub fn clear_all_revalidation(&mut self) {
        self.revalidating.clear();
    }

    /// Remove all revalidation flags and return the keys.
    ///
    /// Useful when failed providers should have their cached data
    /// invalidated rather than preserved.
    pub fn drain_revalidation(&mut self) -> Vec<K> {
        self.revalidating.drain().collect()
    }

    /// Reload the cache from disk, replacing all in-memory data.
    pub fn reload(&mut self) {
        if let Some(data) = Self::load_from_disk(&self.file_path) {
            self.data = data;
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Load the cache from the JSON file at `path`.
    /// Returns `None` if the file does not exist, is corrupted, or unreadable.
    fn load_from_disk(path: &PathBuf) -> Option<HashMap<K, V>> {
        let content = std::fs::read_to_string(path).ok()?;
        let envelope: CacheEnvelope<K, V> = serde_json::from_str(&content).ok()?;
        Some(envelope.data)
    }

    /// Persist the current in-memory data to disk as JSON.
    fn save(&self) {
        let envelope = CacheEnvelope {
            data: self.data.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&envelope) {
            std::fs::write(&self.file_path, json).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic_store_and_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.json");
        let mut cache = StaleCache::<String, String>::new_for_test(file_path);

        assert!(cache.get(&"key1".to_string()).is_none());

        cache.finish_revalidation("key1".to_string(), "value1".to_string());
        assert_eq!(cache.get(&"key1".to_string()), Some(&"value1".to_string()));
    }

    #[test]
    fn test_cache_revalidation_flag() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.json");
        let mut cache = StaleCache::<String, String>::new_for_test(file_path);

        assert!(!cache.is_revalidating(&"key1".to_string()));
        assert!(cache.start_revalidation("key1".to_string()));
        // Second call should return false
        assert!(!cache.start_revalidation("key1".to_string()));
        assert!(cache.is_revalidating(&"key1".to_string()));

        cache.finish_revalidation("key1".to_string(), "value1".to_string());
        assert!(!cache.is_revalidating(&"key1".to_string()));
    }

    #[test]
    fn test_cache_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.json");
        let mut cache = StaleCache::<String, String>::new_for_test(file_path);

        cache.finish_revalidation("key1".to_string(), "value1".to_string());
        assert!(cache.get(&"key1".to_string()).is_some());

        cache.invalidate(&"key1".to_string());
        assert!(cache.get(&"key1".to_string()).is_none());
    }

    #[test]
    fn test_cache_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("persist.json");

        // First cache: store data
        {
            let mut cache = StaleCache::<String, String>::new_for_test(file_path.clone());
            cache.finish_revalidation("key1".to_string(), "value1".to_string());
            cache.finish_revalidation("key2".to_string(), "value2".to_string());
        } // drops, should have persisted

        // Second cache: load from same file
        {
            let cache = StaleCache::<String, String>::new_for_test(file_path);
            assert_eq!(cache.get(&"key1".to_string()), Some(&"value1".to_string()));
            assert_eq!(cache.get(&"key2".to_string()), Some(&"value2".to_string()));
        }
    }

    #[test]
    fn test_cache_corrupted_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("corrupt.json");
        std::fs::write(&file_path, "this is not valid json").unwrap();

        // Should not panic — treats corrupted file as empty cache
        let cache = StaleCache::<String, String>::new_for_test(file_path);
        assert!(cache.get(&"any".to_string()).is_none());
    }

    #[test]
    fn test_cache_missing_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("missing.json");

        // File does not exist — should not panic
        let cache = StaleCache::<String, String>::new_for_test(file_path);
        assert!(cache.get(&"any".to_string()).is_none());
    }

    // Test helper: create cache with an explicit file path (no ProjectDirs).
    impl<K, V> StaleCache<K, V>
    where
        K: Eq + Hash + Clone + Serialize + DeserializeOwned,
        V: Clone + Serialize + DeserializeOwned,
    {
        fn new_for_test(file_path: PathBuf) -> Self {
            // Ensure parent dir exists
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let data = Self::load_from_disk(&file_path).unwrap_or_default();
            Self {
                data,
                revalidating: HashSet::new(),
                file_path,
            }
        }
    }
}
