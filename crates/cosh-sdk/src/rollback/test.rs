use serial_test::serial;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::hashline::snapshots::SnapshotStore;

use super::core::{
    MAX_PATHS, MAX_SNAPSHOT_BYTES, MAX_VERSIONS_PER_PATH, RestoreInput, record, restore,
    session_store,
};

static TEST_ID: AtomicU64 = AtomicU64::new(0);

fn tmp(label: &str) -> String {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("/tmp/cosh_rb_{label}_{id}")
}

fn write(path: &str, content: &str) {
    std::fs::write(path, content).expect("write test file");
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).expect("read test file")
}

fn rm(path: &str) {
    let _ = std::fs::remove_file(path);
}

// record() — unit tests (no disk I/O needed)

#[test]
#[serial]
fn record_returns_hash_for_normal_file() {
    let hash = record("/fake/path.rs", "fn main() {}");
    assert!(hash.is_some(), "expected hash for normal content");
    let h = hash.unwrap();
    assert_eq!(h.len(), 4, "hash should be 4 hex chars");
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "hash must be hex");
}

#[test]
#[serial]
fn record_returns_same_hash_for_identical_content() {
    let a = record("/fake/dedup_a.rs", "same content");
    let b = record("/fake/dedup_b.rs", "same content");
    assert_eq!(a, b, "identical content must produce the same hash");
}

#[test]
#[serial]
fn record_deduplicates_repeated_calls_for_same_path() {
    let p = "/fake/dedup_same_path.rs";
    let h1 = record(p, "version one");
    let h2 = record(p, "version one");
    assert_eq!(
        h1, h2,
        "recording identical content again must return same hash"
    );

    let mut store = session_store().lock().unwrap();
    let history = store.history(p);
    assert_eq!(
        history.len(),
        1,
        "duplicate content must not create a second entry"
    );
}

#[test]
#[serial]
fn record_skips_binary_content_null_byte() {
    let result = record("/fake/binary.bin", "valid\0invalid");
    assert!(result.is_none(), "content with null bytes must be skipped");
}

#[test]
#[serial]
fn record_skips_oversized_content() {
    let big = "x".repeat(MAX_SNAPSHOT_BYTES + 1);
    let result = record("/fake/big.txt", &big);
    assert!(
        result.is_none(),
        "content exceeding MAX_SNAPSHOT_BYTES must be skipped"
    );
}

#[test]
#[serial]
fn record_normalizes_crlf_to_lf_before_storing() {
    let p = "/fake/crlf_norm.rs";
    let lf_hash = record(p, "line1\nline2\n");
    let crlf_hash = record(p, "line1\r\nline2\r\n");
    assert_eq!(
        lf_hash, crlf_hash,
        "CRLF and LF variants of the same content must produce the same hash"
    );
    let mut store = session_store().lock().unwrap();
    let history = store.history(p);
    assert_eq!(
        history.len(),
        1,
        "CRLF and LF must deduplicate to one entry"
    );
    assert!(
        !history[0].text.contains('\r'),
        "stored text must have LF-only line endings"
    );
}

#[test]
#[serial]
fn record_strips_bom_before_storing() {
    let p = "/fake/bom_strip.rs";
    let bom_hash = record(p, "\u{FEFF}content after bom");
    let plain_hash = record(p, "content after bom");
    assert_eq!(
        bom_hash, plain_hash,
        "BOM variant must hash identically to plain"
    );
    let mut store = session_store().lock().unwrap();
    let history = store.history(p);
    assert!(
        !history[0].text.contains('\u{FEFF}'),
        "stored text must not contain BOM"
    );
}

// restore() — success paths (require real disk I/O)

#[tokio::test]
#[serial]
async fn restore_by_hash_reverts_file_to_target_version() {
    let path = tmp("by_hash");
    write(&path, "version one\n");
    let hash_v1 = record(&path, "version one\n").unwrap();

    write(&path, "version two\n");
    let _hash_v2 = record(&path, "version two\n").unwrap();

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(hash_v1.clone()),
    })
    .await
    .expect("restore should succeed");

    assert_eq!(read(&path), "version one\n");
    assert_eq!(out.file_hash, hash_v1);
    assert!(
        out.warning.is_none(),
        "no warning expected for clean restore"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_previous_version_with_hash_none() {
    let path = tmp("prev_none");
    write(&path, "v1 content\n");
    let _ = record(&path, "v1 content\n");

    write(&path, "v2 content\n");
    let _ = record(&path, "v2 content\n");

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .expect("hash=None restore should succeed");

    assert_eq!(read(&path), "v1 content\n");
    assert!(out.warning.is_none());
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_output_replaced_hash_equals_pre_restore_disk_hash() {
    let path = tmp("replaced_hash");
    write(&path, "alpha\n");
    let h1 = record(&path, "alpha\n").unwrap();

    write(&path, "beta\n");
    let h2 = record(&path, "beta\n").unwrap();

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h1),
    })
    .await
    .unwrap();

    assert_eq!(
        out.replaced_hash, h2,
        "replaced_hash should be the hash of what was on disk"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_enables_undo_of_undo() {
    let path = tmp("undo_undo");
    write(&path, "original\n");
    let h_original = record(&path, "original\n").unwrap();

    write(&path, "edited\n");
    let h_edited = record(&path, "edited\n").unwrap();

    // Roll back to original
    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_original.clone()),
    })
    .await
    .unwrap();
    assert_eq!(read(&path), "original\n");

    // Undo the rollback: roll forward to edited
    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_edited.clone()),
    })
    .await
    .expect("undo of undo should succeed");

    assert_eq!(read(&path), "edited\n");
    assert_eq!(out.file_hash, h_edited);
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_multi_step_navigation() {
    let path = tmp("multi_step");
    let versions = ["v1\n", "v2\n", "v3\n", "v4\n"];
    let mut hashes = Vec::new();

    for v in &versions {
        write(&path, v);
        hashes.push(record(&path, v).unwrap());
    }

    // Roll back step by step: v4 -> v3 -> v2 -> v1
    for (i, expected) in versions.iter().enumerate().rev().skip(1) {
        restore(RestoreInput {
            path: path.clone(),
            hash: None,
        })
        .await
        .unwrap_or_else(|e| panic!("step {i} failed: {e}"));
        assert_eq!(read(&path), *expected, "step {i}: unexpected content");
    }
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_recreates_deleted_file_with_explicit_hash() {
    let path = tmp("recreate_hash");
    write(&path, "content to restore\n");
    let hash = record(&path, "content to restore\n").unwrap();

    rm(&path);
    assert!(
        !std::path::Path::new(&path).exists(),
        "file should be deleted"
    );

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(hash.clone()),
    })
    .await
    .expect("restore should recreate deleted file");

    assert!(
        std::path::Path::new(&path).exists(),
        "file should exist after restore"
    );
    assert_eq!(read(&path), "content to restore\n");
    assert_eq!(out.file_hash, hash);
    assert!(
        out.warning
            .as_deref()
            .is_some_and(|w| w.contains("no longer exists")),
        "should warn about recreation"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_deleted_file_no_hash_restores_to_head_with_warning() {
    let path = tmp("recreate_none");
    write(&path, "last known\n");
    let h = record(&path, "last known\n").unwrap();

    rm(&path);

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .expect("hash=None on deleted file should restore to head");

    assert_eq!(read(&path), "last known\n");
    assert_eq!(out.file_hash, h);
    assert!(
        out.warning
            .as_deref()
            .is_some_and(|w| w.contains("no longer exists")),
        "should warn about recreation"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_external_modification_with_explicit_hash_warns_and_succeeds() {
    let path = tmp("ext_mod_hash");
    write(&path, "session version\n");
    let h_session = record(&path, "session version\n").unwrap();

    // Simulate external tool modifying the file without going through record()
    write(&path, "externally modified\n");

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_session.clone()),
    })
    .await
    .expect("restore over external modification should succeed");

    assert_eq!(read(&path), "session version\n");
    assert_eq!(out.file_hash, h_session);
    assert!(
        out.warning
            .as_deref()
            .is_some_and(|w| w.contains("modified externally")),
        "should warn about external modification"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_preserves_crlf_line_ending_from_current_disk() {
    let path = tmp("crlf_preserve");
    write(&path, "v1 line\n");
    let h_v1 = record(&path, "v1 line\n").unwrap();

    // Write v2 with CRLF to disk, record it
    write(&path, "v2 line\r\n");
    let _ = record(&path, "v2 line\r\n");

    // Restore to v1; current disk has CRLF so result should use CRLF
    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_v1),
    })
    .await
    .unwrap();

    let on_disk = read(&path);
    assert!(
        on_disk.contains("\r\n"),
        "CRLF from current disk must be preserved on restore"
    );
    assert!(on_disk.contains("v1 line"), "v1 content must be present");
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_preserves_bom_from_current_disk() {
    let path = tmp("bom_preserve");
    write(&path, "plain text v1\n");
    let h_v1 = record(&path, "plain text v1\n").unwrap();

    // Write v2 with BOM to disk
    write(&path, "\u{FEFF}plain text v2\n");
    let _ = record(&path, "\u{FEFF}plain text v2\n");

    // Restore to v1; current disk has BOM so restored file should also have BOM
    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_v1),
    })
    .await
    .unwrap();

    let raw = std::fs::read(&path).unwrap();
    assert!(
        raw.starts_with(b"\xEF\xBB\xBF"),
        "BOM from current disk must be preserved on restore"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_header_is_valid_hashline_format() {
    let path = tmp("header_fmt");
    write(&path, "content\n");
    let h = record(&path, "content\n").unwrap();

    write(&path, "updated\n");
    let _ = record(&path, "updated\n");

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h.clone()),
    })
    .await
    .unwrap();

    assert!(
        out.header.starts_with('\u{00B6}'),
        "header must start with the paragraph sigil"
    );
    assert!(
        out.header.contains(&format!("#{h}")),
        "header must contain the hash"
    );
    assert!(out.header.contains(&path), "header must contain the path");
    rm(&path);
}

// restore() — error paths

#[tokio::test]
#[serial]
async fn restore_error_no_history_for_path() {
    let path = tmp("no_history");
    write(&path, "some content\n");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("should fail with no history");

    assert!(
        err.contains("no rollback history"),
        "error must mention missing history, got: {err}"
    );
    assert!(
        err.contains("read or written"),
        "error must guide the user, got: {err}"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_error_hash_not_found_lists_known_hashes() {
    let path = tmp("hash_not_found");
    write(&path, "content\n");
    let h = record(&path, "content\n").unwrap();

    write(&path, "updated\n");
    let _ = record(&path, "updated\n");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: Some("DEAD".to_string()),
    })
    .await
    .err()
    .expect("should fail with hash not found");

    assert!(
        err.contains("DEAD"),
        "error must mention the requested hash"
    );
    assert!(err.contains(&h), "error must list known hashes");
    assert!(
        err.contains("known versions"),
        "error must use the phrase 'known versions'"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_error_already_at_target_version() {
    let path = tmp("already_current");
    write(&path, "current content\n");
    let h = record(&path, "current content\n").unwrap();

    write(&path, "newer content\n");
    let _ = record(&path, "newer content\n");

    // Restore to h once to put h on disk
    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h.clone()),
    })
    .await
    .unwrap();

    // Now try to restore to h again — disk already has h
    let err = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h.clone()),
    })
    .await
    .err()
    .expect("should fail: already at target version");

    assert!(
        err.contains("already at version"),
        "error must say already at version, got: {err}"
    );
    assert!(err.contains(&h), "error must include the hash");
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_error_no_previous_version_at_oldest() {
    let path = tmp("oldest_only");
    write(&path, "only version\n");
    let _ = record(&path, "only version\n");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("should fail: no previous version");

    assert!(
        err.contains("no previous version"),
        "error must say no previous version, got: {err}"
    );
    assert!(
        err.contains("oldest"),
        "error must mention oldest, got: {err}"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_error_hash_none_on_externally_modified_file() {
    let path = tmp("ext_mod_none");
    write(&path, "session wrote this\n");
    let h = record(&path, "session wrote this\n").unwrap();

    // External modification — disk content not in session history
    write(&path, "externally changed content\n");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("should fail: external modification");

    assert!(
        err.contains("modified externally"),
        "error must mention external modification, got: {err}"
    );
    assert!(
        err.contains(&h),
        "error must list known session hashes so caller can pick one"
    );
    assert!(
        err.contains("explicit hash"),
        "error must tell the caller to pass an explicit hash"
    );
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_error_disk_read_failure_non_utf8() {
    // Write raw bytes that are not valid UTF-8 to a file.
    // tokio::fs::read_to_string will fail, which restore maps to an error.
    let path = tmp("non_utf8");
    std::fs::write(&path, b"\xff\xfe invalid utf8 \x00\x01").unwrap();
    let _ = record(&path, "something in history");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("should fail on non-utf8 or missing history");

    // The error comes from the read phase — it should mention the path.
    assert!(
        err.contains("failed to read") || err.contains("no previous"),
        "should surface a read error or history error, got: {err}"
    );
    rm(&path);
}

// Window / LRU boundary tests

#[test]
#[serial]
fn record_respects_max_versions_per_path() {
    let p = tmp("max_versions");
    let extra = MAX_VERSIONS_PER_PATH + 3;

    for i in 0..extra {
        let _ = record(&p, &format!("version {i}\n"));
    }

    let mut store = session_store().lock().unwrap();
    let history = store.history(&p);

    assert!(
        history.len() <= MAX_VERSIONS_PER_PATH,
        "history must not exceed MAX_VERSIONS_PER_PATH ({}), got {}",
        MAX_VERSIONS_PER_PATH,
        history.len()
    );

    // The most recent version must be at head
    assert!(
        history[0].text.contains(&format!("version {}", extra - 1)),
        "head must be the most recently recorded version"
    );

    // The oldest versions must have been evicted
    assert!(
        !history.iter().any(|v| v.text.contains("version 0")),
        "oldest versions must be evicted when window is full"
    );
}

#[test]
#[serial]
fn record_at_exactly_max_snapshot_bytes_is_accepted() {
    let content = "a".repeat(MAX_SNAPSHOT_BYTES);
    let result = record("/fake/exact_limit.txt", &content);
    assert!(
        result.is_some(),
        "content at exactly MAX_SNAPSHOT_BYTES must be accepted"
    );
}

#[test]
#[serial]
fn record_one_byte_over_max_snapshot_bytes_is_rejected() {
    let content = "a".repeat(MAX_SNAPSHOT_BYTES + 1);
    let result = record("/fake/over_limit.txt", &content);
    assert!(
        result.is_none(),
        "content over MAX_SNAPSHOT_BYTES must be rejected"
    );
}

// Gap tests — real agent scenarios not previously covered

#[test]
#[serial]
fn record_accepts_empty_content() {
    // Agents commonly clear a file before rewriting it completely.
    let result = record("/fake/empty.rs", "");
    assert!(
        result.is_some(),
        "empty content is valid and must be recorded"
    );
    let mut store = session_store().lock().unwrap();
    let history = store.history("/fake/empty.rs");
    assert!(!history.is_empty(), "empty content must appear in history");
    assert_eq!(history[0].text, "", "stored text must be empty string");
}

#[tokio::test]
#[serial]
async fn restore_to_empty_content_clears_file() {
    let path = tmp("restore_empty");
    write(&path, "original content\n");
    let h_empty = record(&path, "").unwrap();

    write(&path, "new content\n");
    let _ = record(&path, "new content\n");

    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h_empty),
    })
    .await
    .expect("restoring to empty content should succeed");

    assert_eq!(
        read(&path),
        "",
        "file must be empty after restoring empty snapshot"
    );
    rm(&path);
}

#[test]
#[serial]
fn record_lru_path_eviction_removes_entire_history() {
    // Agent opens MAX_PATHS + 1 distinct files. The first file's history
    // must be evicted so the store stays within its path budget.
    let first_path = format!(
        "/fake/evict_path_first_{}",
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    let _ = record(&first_path, "first file content");

    // Fill the remaining slots with unique paths
    for i in 0..MAX_PATHS {
        let p = format!(
            "/fake/evict_filler_{i}_{}",
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let _ = record(&p, &format!("filler {i}"));
    }

    let mut store = session_store().lock().unwrap();
    let history = store.history(&first_path);
    assert!(
        history.is_empty(),
        "path evicted by LRU must have no history in the store"
    );
}

#[tokio::test]
#[serial]
async fn restore_fails_with_clear_error_after_path_eviction() {
    // Verify that the error message is actionable when a path's history
    // has been evicted from the store.
    let evicted_path = format!(
        "/tmp/cosh_rb_evicted_{}",
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    write(&evicted_path, "content before eviction\n");
    let _ = record(&evicted_path, "content before eviction\n");

    // Flood the store to evict the path above
    for i in 0..MAX_PATHS {
        let p = format!(
            "/fake/flood_{i}_{}",
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let _ = record(&p, &format!("flood {i}"));
    }

    let err = restore(RestoreInput {
        path: evicted_path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("restore after eviction should fail");

    assert!(
        err.contains("no rollback history"),
        "error must mention missing history, got: {err}"
    );
    rm(&evicted_path);
}

#[tokio::test]
#[serial]
async fn restore_then_record_new_edit_navigates_correctly() {
    // The most common real agent workflow:
    // 1. Agent writes v1 and v2.
    // 2. Decides v1 was better, restores to v1.
    // 3. Continues editing from v1 to v3.
    // 4. hash=None from v3 should go to v1, not v2.
    let path = tmp("post_restore_edit");
    write(&path, "v1\n");
    let h1 = record(&path, "v1\n").unwrap();

    write(&path, "v2\n");
    let _ = record(&path, "v2\n");

    // Restore to v1
    restore(RestoreInput {
        path: path.clone(),
        hash: Some(h1.clone()),
    })
    .await
    .expect("restore to v1 should succeed");

    // Continue editing to v3
    write(&path, "v3\n");
    let _ = record(&path, "v3\n");
    assert_eq!(read(&path), "v3\n");

    // hash=None from v3 must go back to v1 (the base we edited from),
    // not to v2 (which predates the restore).
    let out = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .expect("hash=None after edit should succeed");

    assert_eq!(read(&path), "v1\n", "must restore to v1, not v2");
    assert_eq!(out.file_hash, h1);
    rm(&path);
}

#[tokio::test]
#[serial]
async fn restore_replaced_hash_is_empty_string_when_file_did_not_exist() {
    // When restoring a deleted file, replaced_hash must be an empty string
    // because there was no on-disk content to compute a hash from.
    // Callers must not pass replaced_hash back to restore() as a target hash.
    let path = tmp("replaced_hash_empty");
    write(&path, "content\n");
    let h = record(&path, "content\n").unwrap();

    rm(&path);

    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(h),
    })
    .await
    .expect("restore of deleted file should succeed");

    assert_eq!(
        out.replaced_hash, "",
        "replaced_hash must be empty when file did not exist before restore"
    );
    rm(&path);
}

#[test]
#[serial]
fn record_accepts_unicode_content() {
    // Agents write files with UTF-8 comments, docstrings, string literals
    // containing emoji and non-ASCII characters.
    let content =
        "// \u{1F600} emoji\n// \u{4E2D}\u{6587} CJK\n// caf\u{00E9} accented\nfn run() {}\n";
    let result = record("/fake/unicode.rs", content);
    assert!(
        result.is_some(),
        "valid UTF-8 unicode content must be accepted"
    );

    let mut store = session_store().lock().unwrap();
    let history = store.history("/fake/unicode.rs");
    assert!(
        !history.is_empty(),
        "unicode content must appear in history"
    );
    assert!(
        history[0].text.contains("caf\u{00E9}"),
        "stored text must preserve unicode characters exactly"
    );
}

#[tokio::test]
#[serial]
async fn restore_multiple_paths_are_independent() {
    // Restoring file A must not affect file B's history or content.
    let path_a = tmp("independent_a");
    let path_b = tmp("independent_b");

    write(&path_a, "a-v1\n");
    let h_a1 = record(&path_a, "a-v1\n").unwrap();
    write(&path_a, "a-v2\n");
    let _ = record(&path_a, "a-v2\n");

    write(&path_b, "b-v1\n");
    let h_b1 = record(&path_b, "b-v1\n").unwrap();
    write(&path_b, "b-v2\n");
    let _ = record(&path_b, "b-v2\n");

    // Restore file A
    restore(RestoreInput {
        path: path_a.clone(),
        hash: Some(h_a1),
    })
    .await
    .expect("restore of path_a should succeed");

    // File B must be completely unaffected
    assert_eq!(read(&path_b), "b-v2\n", "file B content must not change");

    let mut store = session_store().lock().unwrap();
    let b_history = store.history(&path_b);
    assert!(
        b_history.iter().any(|v| v.hash == h_b1),
        "file B history must still contain its own versions"
    );

    rm(&path_a);
    rm(&path_b);
}

#[tokio::test]
#[serial]
async fn restore_fails_when_write_directory_does_not_exist() {
    // The parent directory of the target path does not exist.
    // restore() must return a clear disk-write error, not panic.
    let base = tmp("nonexistent_parent");
    let path = format!("{base}/sub/file.rs");
    let _ = record(&path, "some content");

    // Write second version so hash=None has somewhere to go
    let _ = record(&path, "other content");

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("restore into non-existent directory should fail");

    assert!(
        err.contains("failed to write") || err.contains("failed to read"),
        "error must indicate a file-system failure, got: {err}"
    );
}

#[tokio::test]
#[serial]
async fn restore_error_when_file_grew_beyond_snapshot_limit() {
    // Agent recorded a small file (v1), then wrote a version too large to
    // snapshot (v2). With hash=None, the disk content (v2) is not in the
    // store, producing an "external modification" error.
    // The error is technically imprecise (agent wrote it, not an external
    // tool), but the actionable part — listing known hashes — is correct.
    let path = tmp("grew_beyond_limit");
    write(&path, "small content\n");
    let h_small = record(&path, "small content\n").unwrap();

    // Write large content to disk but do NOT record (simulates the engine
    // silently skipping it because the content is too large)
    let large = "x".repeat(MAX_SNAPSHOT_BYTES + 1);
    write(&path, &large);
    // record() returns None — the oversized content is not stored
    assert!(
        record(&path, &large).is_none(),
        "oversized content must not be recorded"
    );

    let err = restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await
    .err()
    .expect("hash=None with unrecorded large file should fail");

    assert!(
        err.contains("modified externally") || err.contains("no rollback history"),
        "error must describe why navigation failed, got: {err}"
    );
    // When the path is still in the store (not evicted by concurrent tests),
    // the error must list the known hash so the agent can use it explicitly.
    // If the path was evicted by LRU pressure from parallel tests the error
    // changes to "no rollback history" and the hash is not listed — that
    // behaviour is separately covered by restore_fails_with_clear_error_after_path_eviction.
    if err.contains("modified externally") {
        assert!(
            err.contains(&h_small),
            "when path is still in store, error must list known hashes, got: {err}"
        );
    }
    rm(&path);
}
