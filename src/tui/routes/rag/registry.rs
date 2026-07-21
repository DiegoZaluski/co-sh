//! Registry of known vector databases for the RAG feature.
//!
//! Persisted as JSON in `$DATA_DIR/cosh/rag_dbs.json`.
//! Each entry stores metadata about a LanceDB database: its name, path on disk,
//! description (user-provided), and the embedding model used.

use std::collections::HashSet;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

use super::models::RagDb;

/// The full registry of all known RAG databases.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RagRegistry {
    pub dbs: Vec<RagDb>,
}

impl RagRegistry {
    /// Load registry from disk. Returns empty registry if file doesn't exist.
    pub fn load() -> Self {
        let path = Self::registry_path();
        if !path.exists() {
            return Self { dbs: Vec::new() };
        }
        match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("Failed to parse rag_dbs.json: {e}, using empty registry");
                Self { dbs: Vec::new() }
            }),
            Err(e) => {
                log::warn!("Failed to read rag_dbs.json: {e}, using empty registry");
                Self { dbs: Vec::new() }
            }
        }
    }

    /// Save registry to disk.
    pub fn save(&self) {
        let path = Self::registry_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, &json) {
                    log::error!("Failed to write rag_dbs.json: {e}");
                }
            }
            Err(e) => log::error!("Failed to serialize rag_dbs.json: {e}"),
        }
    }

    /// Find a DB by name.
    pub fn find(&self, name: &str) -> Option<&RagDb> {
        self.dbs.iter().find(|db| db.name == name)
    }

    /// Find a DB by name (mutable).
    pub fn find_mut(&mut self, name: &str) -> Option<&mut RagDb> {
        self.dbs.iter_mut().find(|db| db.name == name)
    }

    /// Add or update a DB. If a DB with the same name exists, it is replaced.
    /// Returns `true` if the DB already existed (update), `false` if new.
    pub fn upsert(&mut self, db: RagDb) -> bool {
        let existed = self.dbs.iter().any(|d| d.name == db.name);
        self.dbs.retain(|d| d.name != db.name);
        self.dbs.push(db);
        self.save();
        existed
    }

    /// Remove a DB by name.
    pub fn remove(&mut self, name: &str) {
        self.dbs.retain(|d| d.name != name);
        self.save();
    }

    /// Get the set of active DB names (from the UI toggle state).
    /// Persisted separately from the registry itself.
    pub fn load_active() -> HashSet<String> {
        let path = Self::active_path();
        if !path.exists() {
            return HashSet::new();
        }
        match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => HashSet::new(),
        }
    }

    /// Save the active DB names.
    pub fn save_active(active: &HashSet<String>) {
        let path = Self::active_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(active) {
            let _ = fs::write(&path, &json);
        }
    }

    /// Build the tool description suffix for the recall_search tool.
    /// Includes only the active databases.
    pub fn build_tool_suffix(&self, active_dbs: &HashSet<String>) -> String {
        let mut suffix = String::new();
        suffix.push_str("Available databases:\n\n");

        let mut any_active = false;
        for db in &self.dbs {
            if active_dbs.contains(&db.name) {
                any_active = true;
                let _ = write!(
                    &mut suffix,
                    "- Name: {}\n  Description: {}\n  DB URI: {}\n  Embedding: {}\n\n",
                    db.name,
                    db.description,
                    db.uri,
                    db.embedder.label(),
                );
            }
        }

        if !any_active {
            suffix.push_str("(none active)\n");
        }

        suffix
    }

    /// Compute the data root directory (platform-specific).
    fn data_dir() -> PathBuf {
        directories::BaseDirs::new()
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cosh")
    }

    /// Path to the registry JSON file.
    fn registry_path() -> PathBuf {
        Self::data_dir().join("rag_dbs.json")
    }

    /// Path to the active DBs file.
    fn active_path() -> PathBuf {
        Self::data_dir().join("rag_active.json")
    }

    /// Compute the URI for a new database with the given name.
    pub fn db_uri(name: &str) -> String {
        Self::data_dir().join(name).to_string_lossy().to_string()
    }

    /// Check if a database name already exists.
    pub fn exists(&self, name: &str) -> bool {
        self.dbs.iter().any(|db| db.name == name)
    }
}

impl Default for RagRegistry {
    fn default() -> Self {
        Self::load()
    }
}
