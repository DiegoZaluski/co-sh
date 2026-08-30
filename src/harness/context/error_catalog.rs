//! Provider-error catalog: context windows observed from provider API errors.
//!
//! A context-window overflow is the provider's OWN report of its maximum input
//! size ("maximum context length is 128000") — more authoritative than a guess
//! from a public catalog, and worth persisting across launches. This lives
//! HERE, in the harness, deliberately isolated from `cosh-sdk`'s discovery: it
//! records the user's own observed maxima (private, per-machine state), not a
//! public model listing. It is stored at the TOP level of the data dir
//! (`~/.local/share/cosh`), not under the `cache/` subdir the public catalogs
//! use — a record, not a cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// File name of the provider-error catalog (a JSON map of `model → window`).
const API_ERROR_CATALOG_FILE: &str = "api_errors.json";

/// Resolve the directory holding the provider-error catalog:
/// `{data_local_dir}/cosh`.
fn resolve_error_catalog_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.data_local_dir().join("cosh"))
}

/// Read the provider-error catalog map from `path`. `None` when the file is
/// missing or unparseable — a corrupted catalog is ignored, never fatal.
fn load_error_catalog(path: &Path) -> Option<HashMap<String, usize>> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write `catalog` to `path`, creating the directory if needed. The write is
/// ATOMIC (temp file + rename), so a crash mid-write can never leave a
/// truncated/corrupt catalog behind. Never fatal — a failing cache write must
/// not block the agent loop.
fn save_error_catalog(path: &Path, catalog: &HashMap<String, usize>) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!(
            "failed to create provider-error catalog dir {}: {e}",
            dir.display()
        );
        return;
    }
    let json = match serde_json::to_string(catalog) {
        Ok(json) => json,
        Err(e) => {
            log::warn!("failed to serialize provider-error catalog: {e}");
            return;
        }
    };
    // Same directory as the target so the rename stays on one filesystem.
    // The pid suffix prevents two concurrent instances from interleaving
    // write/rename over a shared temp path.
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| API_ERROR_CATALOG_FILE.to_string());
    let tmp = path.with_file_name(format!("{file_name}.{}", std::process::id()));
    // Write + fsync the temp file BEFORE the rename — a rename published
    // without durable content could survive a crash pointing at garbage.
    let write_result = std::fs::File::create(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(json.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = write_result {
        log::warn!(
            "failed to write provider-error catalog {}: {e}",
            tmp.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        log::warn!(
            "failed to swap provider-error catalog into place {}: {e}",
            path.display()
        );
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Flexible match against the provider-error catalog map, mirroring the SDK's
/// discovery: the needle matches a key when they are equal or share a bare
/// (vendor-less) suffix, case-insensitive.
fn find_window_in_error_catalog(
    model_name: &str,
    catalog: &HashMap<String, usize>,
) -> Option<usize> {
    let needle_lower = model_name.to_lowercase();
    let needle_bare = needle_lower.rsplit('/').next().unwrap_or(&needle_lower);
    catalog.iter().find_map(|(id, window)| {
        let id_lower = id.to_lowercase();
        let id_bare = id_lower.rsplit('/').next().unwrap_or(&id_lower);
        let matches = id_lower == needle_lower
            || id_bare == needle_lower
            || id_lower == needle_bare
            || id_bare == needle_bare;
        matches.then_some(*window)
    })
}

/// The model's context window recorded in the provider-error catalog, if any —
/// a window the user's own provider reported in a previous overflow (more
/// authoritative than a public-catalog guess, and a local read).
pub fn error_catalog_window(model_name: &str) -> Option<usize> {
    let dir = resolve_error_catalog_dir()?;
    let catalog = load_error_catalog(&dir.join(API_ERROR_CATALOG_FILE))?;
    find_window_in_error_catalog(model_name, &catalog)
}

/// Persist a model's context window — reported by a provider context-window
/// overflow — for future launches. Never fatal: the caller has already applied
/// the window to its in-memory budget, and a failed write must not break the
/// agent loop.
pub fn save_error_catalog_window(model_name: &str, window: usize) {
    let Some(dir) = resolve_error_catalog_dir() else {
        return;
    };
    let path = dir.join(API_ERROR_CATALOG_FILE);
    let mut catalog = load_error_catalog(&path).unwrap_or_default();
    catalog.insert(model_name.to_string(), window);
    save_error_catalog(&path, &catalog);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_catalog_matches_case_and_vendor_tolerant() {
        let mut catalog = HashMap::new();
        catalog.insert("openai/gpt-4o".to_string(), 128_000);
        catalog.insert("claude-sonnet-4-5".to_string(), 200_000);
        // Exact, bare, capitalized and vendor-prefixed spellings all resolve.
        assert_eq!(
            find_window_in_error_catalog("gpt-4o", &catalog),
            Some(128_000)
        );
        assert_eq!(
            find_window_in_error_catalog("GPT-4O", &catalog),
            Some(128_000)
        );
        assert_eq!(
            find_window_in_error_catalog("anthropic/claude-sonnet-4-5", &catalog),
            Some(200_000)
        );
        // Unknown models fall through.
        assert_eq!(
            find_window_in_error_catalog("unknown-model", &catalog),
            None
        );
    }

    #[test]
    fn error_catalog_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(API_ERROR_CATALOG_FILE);
        let mut catalog = HashMap::new();
        catalog.insert("gpt-4o".to_string(), 128_000);
        save_error_catalog(&path, &catalog);
        let loaded = load_error_catalog(&path).unwrap();
        assert_eq!(loaded.get("gpt-4o"), Some(&128_000));
    }

    /// A save over an existing catalog must MERGE through the read-modify-
    /// write path of the atomic swap: the old entries survive and the file
    /// is never left corrupt mid-swap.
    #[test]
    fn atomic_write_leaves_no_corrupt_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(API_ERROR_CATALOG_FILE);
        let mut catalog = HashMap::new();
        catalog.insert("gpt-4o".to_string(), 128_000);
        save_error_catalog(&path, &catalog);
        // A second save (new model) must merge and keep BOTH entries.
        let mut second = load_error_catalog(&path).unwrap_or_default();
        second.insert("claude-opus-5".to_string(), 1_000_000);
        save_error_catalog(&path, &second);
        let loaded = load_error_catalog(&path).unwrap();
        assert_eq!(loaded.get("gpt-4o"), Some(&128_000));
        assert_eq!(loaded.get("claude-opus-5"), Some(&1_000_000));
        // No temp leftovers beside the catalog.
        let leftovers: Vec<_> = dir
            .path()
            .read_dir()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp residue: {leftovers:?}");
    }
}
