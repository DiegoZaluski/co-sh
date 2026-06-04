//! Storage seam for the hashline patcher. [`Filesystem`] is intentionally
//! minimal — `read_text`, `write_text`, `exists` — so any backing store can be
//! adapted: disk, memory, S3, an LSP text-document protocol, a Git tree, a
//! VFS, etc.
//!
//! The patcher does its own BOM stripping and LF normalization between
//! [`Filesystem::read_text`] and [`Filesystem::write_text`]; the FS deals
//! only in raw text strings.
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Result returned by [`Filesystem::write_text`]. The patcher echoes back
/// `text` so adapters that transform on serialization (e.g. notebooks) can
/// report what actually landed on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteResult {
    /// Final text that was persisted. May differ from the input if the FS transformed it.
    pub text: String,
}

/// ENOENT-like error returned by [`Filesystem::read_text`] when a path is
/// missing. Carrying a `code` property keeps the contract compatible with
/// `std::io::ErrorKind::NotFound` callers that already check
/// `error.kind() == NotFound`.
#[derive(Debug)]
pub struct NotFoundError {
    path: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl NotFoundError {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: None,
        }
    }

    pub fn with_cause(
        path: impl Into<String>,
        cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            path: path.into(),
            source: Some(cause.into()),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for NotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "File not found: {}", self.path)
    }
}

impl std::error::Error for NotFoundError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| &**e as _)
    }
}

/// Type guard for [`NotFoundError`] and structurally-compatible errors.
/// Walks the error chain (transparent through `Box<dyn Error>` wrappers).
pub fn is_not_found(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut err: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = err {
        if e.downcast_ref::<NotFoundError>().is_some() {
            return true;
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::NotFound {
                return true;
            }
        }
        err = e.source();
    }
    false
}

/// Abstract storage backend the [`Patcher`] reads from and writes to.
/// Implement for new backends; this module ships [`InMemoryFilesystem`] and
/// [`DiskFilesystem`] for the most common cases.
///
/// Implementations work with raw text — the patcher handles BOM stripping and
/// line-ending normalization itself. `read_text` MUST return a
/// [`NotFoundError`] (or any error for which [`is_not_found`] returns true)
/// when the path doesn't exist; that's how the patcher detects a create-vs-
/// update.
pub trait Filesystem {
    /// Read the file's full text content. Returns an error on missing file.
    fn read_text(&self, path: &str) -> Result<String>;

    /// Validate that `path` is writable before a prepared batch starts committing.
    fn preflight_write(&self, _path: &str) -> Result<()> {
        Ok(())
    }

    /// Persist `content` at `path`. Returns the actual final text that was written.
    fn write_text(&self, path: &str, content: &str) -> Result<WriteResult>;

    /// Return true when the path exists and can be read. Default: probe via [`read_text`].
    fn exists(&self, path: &str) -> Result<bool> {
        match self.read_text(path) {
            Ok(_) => Ok(true),
            Err(err) => {
                if is_not_found(err.as_ref()) {
                    Ok(false)
                } else {
                    Err(err)
                }
            }
        }
    }

    /// Canonical path used as a key by external caches (e.g. snapshot
    /// stores). The default is identity; override to return an absolute or
    /// otherwise canonicalised path so producers and consumers of cached
    /// snapshots agree on the key without each having to redo the resolution.
    fn canonical_path(&self, path: &str) -> String {
        path.to_string()
    }
}

/// In-memory [`Filesystem`]. Useful for tests, sandboxes, dry-runs, and as
/// a building block for stacked adapters (e.g. an LRU layer on top).
#[derive(Debug, Clone, Default)]
pub struct InMemoryFilesystem {
    files: RefCell<HashMap<String, String>>,
}

impl InMemoryFilesystem {
    pub fn new(initial: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            files: RefCell::new(initial.into_iter().collect()),
        }
    }

    /// Synchronous helper for setting up fixtures.
    pub fn set(&self, path: impl Into<String>, content: impl Into<String>) {
        self.files.borrow_mut().insert(path.into(), content.into());
    }

    /// Synchronous helper for inspecting state.
    pub fn get(&self, path: &str) -> Option<String> {
        self.files.borrow().get(path).cloned()
    }

    /// Remove a single entry. Returns true when something was removed.
    pub fn delete(&self, path: &str) -> bool {
        self.files.borrow_mut().remove(path).is_some()
    }

    /// Wipe all entries.
    pub fn clear(&self) {
        self.files.borrow_mut().clear();
    }

    /// Iterate `(path, content)` pairs.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.files
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Filesystem for InMemoryFilesystem {
    fn read_text(&self, path: &str) -> Result<String> {
        self.files
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| NotFoundError::new(path).into())
    }

    fn write_text(&self, path: &str, content: &str) -> Result<WriteResult> {
        self.files
            .borrow_mut()
            .insert(path.to_string(), content.to_string());
        Ok(WriteResult {
            text: content.to_string(),
        })
    }

    fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.files.borrow().contains_key(path))
    }
}

/// Disk-backed [`Filesystem`] using `std::fs`. The default for CLI
/// use. Paths are accepted as-is; callers responsible for any cwd or
/// jail/sandbox resolution should wrap this with their own implementation.
#[derive(Debug, Clone, Default)]
pub struct DiskFilesystem;

impl DiskFilesystem {
    pub fn new() -> Self {
        Self
    }
}

impl Filesystem for DiskFilesystem {
    fn read_text(&self, path: &str) -> Result<String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(NotFoundError::with_cause(path, err).into())
            }
            Err(err) => Err(err.into()),
        }
    }

    fn write_text(&self, path: &str, content: &str) -> Result<WriteResult> {
        std::fs::write(path, content)?;
        Ok(WriteResult {
            text: content.to_string(),
        })
    }

    /// NOTE: `std::fs::canonicalize` requires the path to exist on disk, unlike the
    /// TS `path.resolve` which is purely lexical. Falls back to the raw path when
    /// canonicalization fails (e.g. file not yet created).
    fn canonical_path(&self, path: &str) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| Path::new(path).to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    fn exists(&self, path: &str) -> Result<bool> {
        Ok(Path::new(path).exists())
    }
}
