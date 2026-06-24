use crate::namespace_cache::{CacheError, NamespaceCache, Verification};
use std::error::Error;
use tempfile::TempDir;

fn make_cache(tmp: &TempDir) -> NamespaceCache {
    let path = tmp.path().join("cache.toml");
    NamespaceCache::new()
        .with_cache_file(path.display().to_string())
}

#[test]
fn verify_returns_modified_when_no_cache() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("server-a", "fs", "file content");

    assert_eq!(cache.verify(), Verification::Modified);
}

#[test]
fn verify_returns_synced_when_cache_matches() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("server-a", "fs", "file content");
    cache.mark_modified("a tool".into());
    cache.flush().unwrap();

    // fresh instance reading the same file
    let mut cache2 = make_cache(&tmp);
    cache2.set_context("server-a", "fs", "file content");
    assert_eq!(cache2.verify(), Verification::Synced);
}

#[test]
fn verify_returns_modified_when_content_differs() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("s", "n", "v1");
    cache.mark_modified("".into());
    cache.flush().unwrap();

    let mut cache2 = make_cache(&tmp);
    cache2.set_context("s", "n", "v2");
    assert_eq!(cache2.verify(), Verification::Modified);
}

#[test]
fn verify_returns_modified_when_entry_unknown() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("server-x", "unknown-ns", "any");

    assert_eq!(cache.verify(), Verification::Modified);
}

#[test]
fn set_context_changes_hash_when_content_changes() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    cache.set_context("s", "n", "hello");
    let hash_a = cache.hash();

    cache.set_context("s", "n", "world");
    let hash_b = cache.hash();

    assert_ne!(hash_a, hash_b);
}

#[test]
fn flush_creates_valid_toml() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("server-a", "fs", "file content");
    cache.mark_modified("a tool".into());
    cache.flush().unwrap();

    let path = tmp.path().join("cache.toml");
    let content = std::fs::read_to_string(path).unwrap();
    assert!(content.starts_with("[server-a.fs]"));
    assert!(content.contains("hash ="));
    assert!(content.contains("description"));
}

#[test]
fn flush_persists_description() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);
    cache.set_context("server-a", "fs", "file content");
    cache.mark_modified("my description".into());
    cache.flush().unwrap();

    // read back and verify
    let mut cache2 = make_cache(&tmp);
    cache2.set_context("server-a", "fs", "file content");
    // should be synced because content + description match the flushed file
    assert_eq!(cache2.verify(), Verification::Synced);
}

#[test]
fn flush_is_noop_when_not_dirty() {
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(&tmp);
    // no mark_modified called
    cache.flush().unwrap();

    let path = tmp.path().join("cache.toml");
    assert!(!path.exists());
}

#[test]
fn multi_entry_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(&tmp);

    // first entry
    cache.set_context("server-a", "fs", "content-1");
    cache.mark_modified("desc-1".into());

    // second entry
    cache.set_context("server-a", "git", "content-2");
    cache.mark_modified("desc-2".into());

    cache.flush().unwrap();

    let mut cache2 = make_cache(&tmp);
    cache2.set_context("server-a", "fs", "content-1");
    assert_eq!(cache2.verify(), Verification::Synced);

    cache2.set_context("server-a", "git", "content-2");
    assert_eq!(cache2.verify(), Verification::Synced);

    cache2.set_context("server-a", "fs", "content-changed");
    assert_eq!(cache2.verify(), Verification::Modified);
}

#[test]
fn with_cache_file_writes_to_custom_path() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("custom-cache.toml");

    let mut cache = NamespaceCache::new()
        .with_cache_file(custom.display().to_string());
    cache.set_context("s", "n", "data");
    cache.mark_modified("desc".into());
    cache.flush().unwrap();

    assert!(custom.exists());
}

#[test]
fn hash_is_zero_before_set_context() {
    let cache = NamespaceCache::new();
    assert_eq!(cache.hash(), 0);
}

#[test]
fn hash_is_nonzero_after_set_context() {
    let mut cache = NamespaceCache::new();
    cache.set_context("s", "n", "some content");
    assert_ne!(cache.hash(), 0);
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
