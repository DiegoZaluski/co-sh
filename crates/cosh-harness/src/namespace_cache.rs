//! On-disk hash cache for tool namespaces — avoids unnecessary re-fetches.
//!
//! [`NamespaceCache`] stores the xxHash of each namespace's content in a local
//! TOML file. On the next run it compares the current hash against the stored
//! one and returns [`Verification::Synced`] (unchanged) or
//! [`Verification::Modified`] (needs refresh).
//!
//! # Quick start
//!
//! ```rust,ignore
//! let mut cache = NamespaceCache::new("server-a", "filesystem", "content", "desc")
//!     .with_cache_file("cache.toml");
//!
//! match cache.run()? {
//!     Verification::Synced => println!("cache is fresh"),
//!     Verification::Modified => {
//!         println!("hash changed, re-fetch needed");
//!         cache.update("new description".into())?;
//!     }
//! }
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

const CACHE_FILE: &str = "cache-namespace-tools.toml";

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
    name_server: String,
    name: String,
    content: String,
    description: String,
    hash: u32,
    cache: HashMap<String, HashMap<String, CacheData>>,
    cache_file: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    Synced,
    Modified,
}

impl NamespaceCache {
    #[must_use]
    pub fn new(
        name_server: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name_server: name_server.into(),
            name: name.into(),
            content: content.into(),
            description: description.into(),
            hash: 0,
            cache: HashMap::new(),
            cache_file: CACHE_FILE.to_string(),
        }
    }

    #[must_use]
    pub fn with_cache_file(mut self, path: impl Into<String>) -> Self {
        self.cache_file = path.into();
        self
    }

    /// Builds a TOML string for the current cache entry.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Serialize`] if serialization fails.
    pub fn build(&mut self) -> Result<String, CacheError> {
        let cache_data = CacheData {
            hash: xxh32(self.content.as_bytes(), 0),
            description: self.description.clone(),
        };
        self.hash = cache_data.hash;

        let mut servers = HashMap::new();
        let mut namespaces = HashMap::new();
        namespaces.insert(self.name.clone(), cache_data);
        servers.insert(self.name_server.clone(), namespaces);

        Ok(toml::to_string_pretty(&servers)?)
    }

    /// Writes a TOML string to the cache file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] if the file cannot be written.
    pub fn set_cache(&self, toml_str: &str) -> Result<(), CacheError> {
        Ok(std::fs::write(&self.cache_file, toml_str)?)
    }

    /// Reads and deserializes the cache file from disk.
    ///
    /// Returns an empty [`HashMap`] if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] if the file cannot be read, or
    /// [`CacheError::Deserialize`] if the content is invalid TOML.
    pub fn get_cache(&self) -> Result<HashMap<String, HashMap<String, CacheData>>, CacheError> {
        let path = Path::new(&self.cache_file);
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Checks whether the cached entry matches the current content.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] or [`CacheError::Deserialize`] if the
    /// cache file cannot be read, or [`CacheError::Serialize`] if the
    /// new entry cannot be built.
    pub fn run(&mut self) -> Result<Verification, CacheError> {
        let old_cache = self.get_cache()?;
        let _ = self.build()?;

        let cached_hash = old_cache
            .get(&self.name_server)
            .and_then(|s| s.get(&self.name))
            .map(|entry| entry.hash);

        match cached_hash {
            Some(old_hash) if old_hash == self.hash => Ok(Verification::Synced),
            _ => {
                self.cache = old_cache;
                Ok(Verification::Modified)
            }
        }
    }

    /// Updates the cache entry with a new description and persists to disk.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Serialize`] if serialization fails, or
    /// [`CacheError::Io`] if the file cannot be written.
    #[must_use]
    pub fn hash(&self) -> u32 {
        self.hash
    }

    pub fn update(&mut self, description: String) -> Result<(), CacheError> {
        if self.hash == 0 {
            self.build()?;
        }

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

        let toml_str = toml::to_string_pretty(&self.cache.clone())?;

        self.set_cache(&toml_str)
    }
}
