use crate::namespace_cache::{CacheError, NamespaceCache, Verification};
use std::error::Error;
use tempfile::TempDir;

fn make_cache(tmp: &TempDir) -> NamespaceCache {
    let path = tmp.path().join("cache.toml");
    NamespaceCache::new("server-a", "fs", "file content", "a tool")
        .with_cache_file(path.display().to_string())
}

#[test]
fn build_returns_valid_toml() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    let toml = cache.build().unwrap();

    assert!(toml.starts_with("[server-a.fs]"), "unexpected toml: {toml}");
    assert!(toml.contains("hash ="));
    assert!(toml.contains("description"));
}

#[test]
fn build_changes_hash_when_content_changes() {
    let tmp = TempDir::new().unwrap();
    let mut a = NamespaceCache::new("s", "n", "hello", "").with_cache_file(tmp.path().join("a.toml").display().to_string());
    let mut b = NamespaceCache::new("s", "n", "world", "").with_cache_file(tmp.path().join("b.toml").display().to_string());

    a.build().unwrap();
    b.build().unwrap();

    assert_ne!(a.hash(), b.hash());
}

#[test]
fn get_cache_returns_empty_when_no_file() {
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(&tmp);

    let result = cache.get_cache().unwrap();

    assert!(result.is_empty());
}

#[test]
fn set_then_get_cache_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    let toml = cache.build().unwrap();
    cache.set_cache(&toml).unwrap();

    let loaded = cache.get_cache().unwrap();
    let entry = loaded
        .get("server-a")
        .and_then(|s| s.get("fs"))
        .unwrap();

    assert_eq!(entry.hash, cache.hash());
    assert_eq!(entry.description, "a tool");
}

#[test]
fn run_returns_modified_when_no_cache() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    let result = cache.run().unwrap();

    assert_eq!(result, Verification::Modified);
}

#[test]
fn run_returns_synced_when_cache_matches() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    let toml = cache.build().unwrap();
    cache.set_cache(&toml).unwrap();

    // fresh instance reading same file
    let mut cache2 = make_cache(&tmp);
    let result = cache2.run().unwrap();

    assert_eq!(result, Verification::Synced);
}

#[test]
fn run_returns_modified_when_content_differs() {
    let tmp = TempDir::new().unwrap();
    let mut cache = NamespaceCache::new("s", "n", "v1", "")
        .with_cache_file(tmp.path().join("c.toml").display().to_string());

    let toml = cache.build().unwrap();
    cache.set_cache(&toml).unwrap();

    // same server+name, but different content hash
    let mut cache2 = NamespaceCache::new("s", "n", "v2", "")
        .with_cache_file(tmp.path().join("c.toml").display().to_string());

    let result = cache2.run().unwrap();
    assert_eq!(result, Verification::Modified);
}

#[test]
fn run_returns_modified_when_entry_unknown() {
    let tmp = TempDir::new().unwrap();
    let mut cache = NamespaceCache::new("server-x", "unknown-ns", "any", "")
        .with_cache_file(tmp.path().join("d.toml").display().to_string());

    let result = cache.run().unwrap();

    assert_eq!(result, Verification::Modified);
}

#[test]
fn update_persists_new_description() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    // first write via run to populate cache
    cache.run().unwrap();
    cache.update("updated description".into()).unwrap();

    // read back
    let loaded = cache.get_cache().unwrap();
    let entry = loaded
        .get("server-a")
        .and_then(|s| s.get("fs"))
        .unwrap();

    assert_eq!(entry.description, "updated description");
    assert_eq!(entry.hash, cache.hash());
}

#[test]
fn update_builds_automatically_if_hash_is_zero() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    // hash is 0, build never called
    assert_eq!(cache.hash(), 0);
    cache.update("desc".into()).unwrap();

    assert_ne!(cache.hash(), 0);
}

#[test]
fn with_cache_file_writes_to_custom_path() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("custom-cache.toml");

    let mut cache = NamespaceCache::new("s", "n", "data", "desc")
        .with_cache_file(custom.display().to_string());

    let toml = cache.build().unwrap();
    cache.set_cache(&toml).unwrap();

    assert!(custom.exists());
}

#[test]
fn cache_error_is_displayed() {
    let err = CacheError::NotFound("missing".into());
    assert_eq!(err.to_string(), "not found: missing");
}

#[test]
fn cache_error_source_returns_inner() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let err = CacheError::Io(io_err);
    assert!(err.source().is_some());
}
