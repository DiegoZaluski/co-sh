//! On-disk hash cache for tool namespaces — avoids unnecessary re-fetches.
//!
//! Uses a **write-back** strategy: the TOML cache is loaded into memory once
//! at construction. All verification and updates happen in memory. At the end
//! of the session call [`flush()`] to persist changes to disk in a single
//! write.
//!
//! # Quick start
//!
//! ```rust,ignore
//! let mut cache = NamespaceCache::new()
//!     .with_cache_file("cache.toml");
//!
//! for tool in tools {
//!     cache.set_context(&tool.server, &tool.ns, &tool.content, &tool.desc);
//!     match cache.verify() {
//!         Verification::Synced => {},
//!         Verification::Modified => {
//!             // re-fetch the namespace, then mark
//!             cache.mark_modified("new description".into());
//!         },
//!     }
//! }
//! cache.flush()?;
//! ```
//!
//! # File format
//!
//! ```toml
//! [server-a.filesystem]
//! hash = 42
//! description = "Filesystem tools"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use xxhash_rust::xxh32::xxh32;

#[derive(Debug)]
pub enum CacheError {
    Io(std::io::Error),
    Serialize(toml::ser::Error),
    Deserialize(toml::de::Error),
    NotFound(String),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Serialize(e) => write!(f, "serialization error: {e}"),
            Self::Deserialize(e) => write!(f, "deserialization error: {e}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Serialize(e) => Some(e),
            Self::Deserialize(e) => Some(e),
            Self::NotFound(_) => None,
        }
    }
}

impl From<std::io::Error> for CacheError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<toml::ser::Error> for CacheError {
    fn from(e: toml::ser::Error) -> Self {
        Self::Serialize(e)
    }
}

impl From<toml::de::Error> for CacheError {
    fn from(e: toml::de::Error) -> Self {
        Self::Deserialize(e)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CacheData {
    pub(crate) hash: u32,
    pub(crate) description: String,
}

pub struct NamespaceCache {
    cache_file: Option<String>,
    cache: HashMap<String, HashMap<String, CacheData>>,
    dirty: bool,
    name_server: String,
    name: String,
    content: String,
    hash: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    Synced,
    Modified,
}

impl NamespaceCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache_file: None,
            cache: HashMap::new(),
            dirty: false,
            name_server: String::new(),
            name: String::new(),
            content: String::new(),
            hash: 0,
        }
    }

    /// Sets the cache file path and loads its contents into memory.
    ///
    /// If the file does not exist or is corrupt, an empty cache is used.
    #[must_use]
    pub fn with_cache_file(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        if Path::new(&path).exists()
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(parsed) = toml::from_str(&content)
        {
            self.cache = parsed;
        }
        self.cache_file = Some(path);
        self
    }

    /// Sets the current tool context and computes its content hash.
    ///
    /// Must be called before [`verify()`] or [`mark_modified()`].
    pub fn set_context(&mut self, name_server: &str, name: &str, content: &str) {
        self.name_server = name_server.to_string();
        self.name = name.to_string();
        self.content = content.to_string();
        self.hash = xxh32(content.as_bytes(), 0);
    }

    /// Checks whether the current tool's content matches the cached hash.
    ///
    /// Pure in-memory operation — no I/O.
    #[must_use]
    pub fn verify(&self) -> Verification {
        match self
            .cache
            .get(&self.name_server)
            .and_then(|s| s.get(&self.name))
        {
            Some(entry) if entry.hash == self.hash => Verification::Synced,
            _ => Verification::Modified,
        }
    }

    /// Updates the in-memory cache entry for the current tool.
    ///
    /// Changes are not written to disk until [`flush()`] is called.
    pub fn mark_modified(&mut self, description: String) {
        let entry = self
            .cache
            .entry(self.name_server.clone())
            .or_default()
            .entry(self.name.clone())
            .or_insert(CacheData {
                hash: 0,
                description: String::new(),
            });
        entry.hash = self.hash;
        entry.description = description;
        self.dirty = true;
    }

    /// Writes the in-memory cache to disk as a single TOML file.
    ///
    /// No-op if no entries have been marked as modified.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Serialize`] if serialization fails, or
    /// [`CacheError::Io`] if the file cannot be written.
    pub fn flush(&self) -> Result<(), CacheError> {
        if !self.dirty {
            return Ok(());
        }
        let Some(ref file) = self.cache_file else {
            return Ok(());
        };
        let toml_str = toml::to_string_pretty(&self.cache)?;
        Ok(std::fs::write(file, &toml_str)?)
    }

    /// Returns a reference to the entire in-memory cache tree.
    ///
    /// Useful for iterating over all cached entries or building context
    /// for LLM prompts without hitting disk.
    ///
    /// ```rust,ignore
    /// for (server, namespaces) in cache.cached() {
    ///     for (name, data) in namespaces {
    ///         prompt.push_str(&format!("[{server}:{name}] {}\n", data.description));
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn cached(&self) -> &HashMap<String, HashMap<String, CacheData>> {
        &self.cache
    }

    /// Looks up a cached description for a given server + namespace.
    ///
    /// Returns `None` if the entry does not exist.
    ///
    /// ```rust,ignore
    /// let summary = cache.get_description("my-server", "filesystem");
    /// ```
    #[must_use]
    pub fn get_description(&self, name_server: &str, name: &str) -> Option<&str> {
        self.cache
            .get(name_server)
            .and_then(|s| s.get(name))
            .map(|entry| entry.description.as_str())
    }

    #[must_use]
    pub fn hash(&self) -> u32 {
        self.hash
    }
}

impl Default for NamespaceCache {
    fn default() -> Self {
        Self::new()
    }
}
