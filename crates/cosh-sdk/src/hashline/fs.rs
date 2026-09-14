//! Storage seam for the hashline patcher. [`Filesystem`] is intentionally
//!
//! minimal — `read_text`, `write_text`, `exists` — so any backing store can be
//! adapted: disk, memory, S3, an LSP text-document protocol, a Git tree, a
//! VFS, etc.
//!
//! The patcher does its own BOM stripping and LF normalization between
//! [`Filesystem::read_text`] and [`Filesystem::write_text`]; the FS deals
//! only in raw text strings.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Result returned by [`Filesystem::write_text`]. The patcher echoes back
/// `text` so adapters that transform on serialization (e.g. notebooks) can
/// report what actually landed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    /// Final text that was persisted. May differ from the input if the FS transformed it.
    pub text: String,
}

/// ENOENT-like error returned by [`Filesystem::read_text`] when a path is
///
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

    #[must_use]
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
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::NotFound
        {
            return true;
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
#[allow(async_fn_in_trait)]
pub trait Filesystem {
    /// Read the file's full text content. Returns an error on missing file.
    async fn read_text(&self, path: &str) -> Result<String>;

    /// Validate that `path` is writable before a prepared batch starts committing.
    async fn preflight_write(&self, _path: &str) -> Result<()> {
        Ok(())
    }

    /// Persist `content` at `path`. Returns the actual final text that was written.
    async fn write_text(&self, path: &str, content: &str) -> Result<WriteResult>;

    /// Return true when the path exists and can be read. Default: probe via [`read_text`].
    async fn exists(&self, path: &str) -> Result<bool> {
        match self.read_text(path).await {
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
    async fn canonical_path(&self, path: &str) -> String {
        path.to_string()
    }
}

/// In-memory [`Filesystem`]. Useful for tests, sandboxes, dry-runs, and as
/// a building block for stacked adapters (e.g. an LRU layer on top).
#[derive(Debug, Default)]
pub struct InMemoryFilesystem {
    files: Mutex<HashMap<String, String>>,
}

#[allow(clippy::unwrap_used)]
impl Clone for InMemoryFilesystem {
    fn clone(&self) -> Self {
        Self {
            files: Mutex::new(self.files.lock().unwrap().clone()),
        }
    }
}

/// Windows `canonicalize` returns verbatim (`\\?\C:\…`) paths. Snapshot keys
/// and headers must stay in the plain drive spelling (matching what every
/// producer/consumer passes), so map `\\?\UNC\server\share` back to
/// `\\server\share` and strip the plain `\\?\` prefix.
#[cfg(windows)]
pub(crate) fn strip_verbatim_prefix(path: std::path::PathBuf) -> std::path::PathBuf {
    use std::path::PathBuf;
    let text = path.as_os_str().to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = text.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

#[cfg(not(windows))]
pub(crate) fn strip_verbatim_prefix(path: std::path::PathBuf) -> std::path::PathBuf {
    path
}

impl InMemoryFilesystem {
    pub fn new(initial: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            files: Mutex::new(initial.into_iter().collect()),
        }
    }

    /// Synchronous helper for setting up fixtures.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn set(&self, path: impl Into<String>, content: impl Into<String>) {
        self.files
            .lock()
            .unwrap()
            .insert(path.into(), content.into());
    }

    /// Synchronous helper for inspecting state.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn get(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }

    /// Remove a single entry. Returns true when something was removed.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn delete(&self, path: &str) -> bool {
        self.files.lock().unwrap().remove(path).is_some()
    }

    /// Wipe all entries.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn clear(&self) {
        self.files.lock().unwrap().clear();
    }

    /// Iterate `(path, content)` pairs.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[allow(clippy::unwrap_used)]
    pub fn entries(&self) -> Vec<(String, String)> {
        self.files
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Filesystem for InMemoryFilesystem {
    #[allow(clippy::unwrap_used)]
    async fn read_text(&self, path: &str) -> Result<String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| NotFoundError::new(path).into())
    }

    #[allow(clippy::unwrap_used)]
    async fn write_text(&self, path: &str, content: &str) -> Result<WriteResult> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), content.to_string());
        Ok(WriteResult {
            text: content.to_string(),
        })
    }

    #[allow(clippy::unwrap_used)]
    async fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.files.lock().unwrap().contains_key(path))
    }
}

/// Disk-backed [`Filesystem`] using `std::fs`. The default for CLI
///
/// use. Paths are accepted as-is; callers responsible for any cwd or
/// jail/sandbox resolution should wrap this with their own implementation.
#[derive(Debug, Clone, Default)]
pub struct DiskFilesystem;

impl DiskFilesystem {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Filesystem for DiskFilesystem {
    async fn read_text(&self, path: &str) -> Result<String> {
        match tokio::fs::read_to_string(path).await {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(NotFoundError::with_cause(path, err).into())
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn write_text(&self, path: &str, content: &str) -> Result<WriteResult> {
        tokio::fs::write(path, content).await?;
        Ok(WriteResult {
            text: content.to_string(),
        })
    }

    async fn canonical_path(&self, path: &str) -> String {
        let canonical = tokio::fs::canonicalize(path)
            .await
            .unwrap_or_else(|_| Path::new(path).to_path_buf());
        // Windows `canonicalize` yields verbatim (`\\?\C:\…`) paths. Snapshot
        // stores are keyed by this string, so a verbatim key would never meet
        // the plain drive spelling every caller (`rollback::record`, guard
        // validation, hashline headers) uses. Keep the plain form.
        strip_verbatim_prefix(canonical)
            .to_string_lossy()
            .to_string()
    }

    async fn exists(&self, path: &str) -> Result<bool> {
        Ok(tokio::fs::try_exists(path).await.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn strip_verbatim_prefix_keeps_snapshot_keys_in_plain_windows_form() {
        use std::path::PathBuf;

        assert_eq!(
            super::strip_verbatim_prefix(PathBuf::from(r"\\?\C:\work\file.rs")),
            PathBuf::from(r"C:\work\file.rs")
        );
        assert_eq!(
            super::strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\file.rs")),
            PathBuf::from(r"\\server\share\file.rs")
        );
    }
}
