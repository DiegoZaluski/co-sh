use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use cosh_sdk::rollback::record;

use super::super::rollback::rollback;
use super::super::types::{FsMetadata, FsRollback};

static TEST_ID: AtomicU64 = AtomicU64::new(0);

const ROOT: &str = "/home/inky/cosh";

fn tmp(label: &str) -> String {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{ROOT}/cosh_test_rb_{label}_{id}.txt")
}

fn meta() -> FsMetadata {
    FsMetadata {
        root: Path::new(ROOT).to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

fn write_file(path: &str, content: &str) {
    std::fs::write(path, content).expect("write test file");
}

fn rm(path: &str) {
    let _ = std::fs::remove_file(path);
}

// Permission enforcement — the tool's primary responsibility

#[tokio::test]
async fn rollback_denied_when_path_is_outside_project_root() {
    let outside = "/tmp/cosh_tool_rb_outside.txt";
    std::fs::write(outside, "content").unwrap();
    let _ = record(outside, "content");

    let err = rollback(&FsRollback, meta(), outside, "")
        .await
        .unwrap_err();

    assert!(
        err.contains("permission denied"),
        "should deny path outside root, got: {err}"
    );
    let _ = std::fs::remove_file(outside);
}

#[tokio::test]
async fn rollback_denied_when_path_is_in_blocklist() {
    let path = tmp("blocked");
    write_file(&path, "v1\n");
    let _ = record(&path, "v1\n");
    write_file(&path, "v2\n");
    let _ = record(&path, "v2\n");

    let blocked_meta = FsMetadata {
        root: Path::new(ROOT).to_path_buf(),
        allowlist: None,
        blocklist: Some(vec![Path::new(ROOT).to_path_buf()]),
    };

    let err = rollback(&FsRollback, blocked_meta, &path, "")
        .await
        .unwrap_err();

    assert!(
        err.contains("permission denied"),
        "blocked path must be denied, got: {err}"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_returns_error_on_blocklist_allowlist_mismatch() {
    let path = tmp("mismatch");
    write_file(&path, "content\n");
    let _ = record(&path, "content\n");

    let mismatch_meta = FsMetadata {
        root: Path::new(ROOT).to_path_buf(),
        allowlist: Some(vec![Path::new(&path).to_path_buf()]),
        blocklist: Some(vec![Path::new(&path).to_path_buf()]),
    };

    let result = rollback(&FsRollback, mismatch_meta, &path, "").await;
    assert!(result.is_err(), "both-list mismatch must return an error");
    rm(&path);
}

#[tokio::test]
async fn rollback_allowed_outside_root_when_path_in_allowlist() {
    let outside = format!(
        "/tmp/cosh_tool_rb_allowlist_{}.txt",
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::write(&outside, "v1\n").unwrap();
    let _ = record(&outside, "v1\n");
    std::fs::write(&outside, "v2\n").unwrap();
    let _ = record(&outside, "v2\n");

    let allowed_meta = FsMetadata {
        root: Path::new(ROOT).to_path_buf(),
        allowlist: Some(vec![Path::new(&outside).to_path_buf()]),
        blocklist: None,
    };

    let result = rollback(&FsRollback, allowed_meta, &outside, "").await;
    assert!(
        result.is_ok(),
        "explicitly allowed path outside root must succeed, got: {:?}",
        result.err()
    );
    let _ = std::fs::remove_file(&outside);
}

// Schema conversion — hash field interpretation

#[tokio::test]
async fn rollback_empty_hash_restores_previous_version() {
    let path = tmp("empty_hash");
    write_file(&path, "version one\n");
    let _ = record(&path, "version one\n");
    write_file(&path, "version two\n");
    let _ = record(&path, "version two\n");

    let out = rollback(&FsRollback, meta(), &path, "")
        .await
        .expect("empty hash must trigger previous-version restore");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "version one\n",
        "empty hash must restore the preceding version"
    );
    assert!(!out.file_hash.is_empty());
    rm(&path);
}

#[tokio::test]
async fn rollback_whitespace_only_hash_treated_as_empty() {
    let path = tmp("ws_hash");
    write_file(&path, "v1\n");
    let _ = record(&path, "v1\n");
    write_file(&path, "v2\n");
    let _ = record(&path, "v2\n");

    let out = rollback(&FsRollback, meta(), &path, "   ")
        .await
        .expect("whitespace-only hash must be treated as empty");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "v1\n",
        "whitespace hash must restore the preceding version"
    );
    drop(out);
    rm(&path);
}

#[tokio::test]
async fn rollback_hash_with_surrounding_whitespace_is_trimmed() {
    let path = tmp("trim_hash");
    write_file(&path, "original\n");
    let h = record(&path, "original\n").unwrap();
    write_file(&path, "updated\n");
    let _ = record(&path, "updated\n");

    let padded = format!("  {h}  ");
    let out = rollback(&FsRollback, meta(), &path, &padded)
        .await
        .expect("hash with surrounding whitespace must be trimmed and resolved");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "original\n",
        "restore must succeed after trimming hash whitespace"
    );
    assert_eq!(out.file_hash, h);
    rm(&path);
}

#[tokio::test]
async fn rollback_explicit_hash_restores_target_version() {
    let path = tmp("explicit_hash");
    write_file(&path, "state a\n");
    let h_a = record(&path, "state a\n").unwrap();
    write_file(&path, "state b\n");
    let _ = record(&path, "state b\n");

    let out = rollback(&FsRollback, meta(), &path, &h_a)
        .await
        .expect("explicit hash restore must succeed");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "state a\n");
    assert_eq!(
        out.file_hash, h_a,
        "file_hash must equal the restored version"
    );
    rm(&path);
}

// Result contract — shape of the output the LLM receives

#[tokio::test]
async fn rollback_header_has_hashline_format() {
    let path = tmp("header_shape");
    write_file(&path, "v1\n");
    let h = record(&path, "v1\n").unwrap();
    write_file(&path, "v2\n");
    let _ = record(&path, "v2\n");

    let out = rollback(&FsRollback, meta(), &path, &h).await.unwrap();

    assert!(
        out.header.starts_with('\u{00B6}'),
        "header must start with the paragraph sigil ¶"
    );
    assert!(
        out.header.contains(&path),
        "header must contain the file path"
    );
    assert!(
        out.header.contains(&format!("#{h}")),
        "header must contain the restored hash"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_replaced_hash_reflects_content_before_restore() {
    let path = tmp("replaced_hash");
    write_file(&path, "before\n");
    let _ = record(&path, "before\n");
    write_file(&path, "after\n");
    let h_after = record(&path, "after\n").unwrap();

    let out = rollback(&FsRollback, meta(), &path, "").await.unwrap();

    assert_eq!(
        out.replaced_hash, h_after,
        "replaced_hash must be the hash of what was on disk before the restore"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_replaced_hash_is_empty_when_file_did_not_exist() {
    let path = tmp("replaced_hash_deleted");
    write_file(&path, "content\n");
    let h = record(&path, "content\n").unwrap();
    rm(&path);

    let out = rollback(&FsRollback, meta(), &path, &h)
        .await
        .expect("restore of deleted file must succeed");

    assert_eq!(
        out.replaced_hash, "",
        "replaced_hash must be empty when the file did not exist before restore"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_warning_is_none_on_clean_restore() {
    let path = tmp("clean_restore");
    write_file(&path, "v1\n");
    let _ = record(&path, "v1\n");
    write_file(&path, "v2\n");
    let _ = record(&path, "v2\n");

    let out = rollback(&FsRollback, meta(), &path, "").await.unwrap();

    assert!(
        out.warning.is_none(),
        "clean restore must not emit a warning"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_warning_is_some_when_file_was_externally_modified() {
    let path = tmp("ext_mod_warning");
    write_file(&path, "session version\n");
    let h = record(&path, "session version\n").unwrap();

    // Simulate external modification — write directly without record()
    write_file(&path, "externally changed\n");

    let out = rollback(&FsRollback, meta(), &path, &h)
        .await
        .expect("restore over external modification must succeed");

    assert!(
        out.warning.is_some(),
        "external modification must produce a warning"
    );
    assert!(
        out.warning
            .as_deref()
            .is_some_and(|w| w.contains("externally")),
        "warning must mention external modification"
    );
    rm(&path);
}

// Error propagation — engine errors surface correctly through the tool layer

#[tokio::test]
async fn rollback_error_no_history_for_path() {
    let path = tmp("no_history_tool");
    write_file(&path, "fresh file\n");

    let err = rollback(&FsRollback, meta(), &path, "").await.unwrap_err();

    assert!(
        err.contains("no rollback history"),
        "engine error must be forwarded as-is, got: {err}"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_error_hash_not_found_in_history() {
    let path = tmp("bad_hash_tool");
    write_file(&path, "content\n");
    let h = record(&path, "content\n").unwrap();
    write_file(&path, "updated\n");
    let _ = record(&path, "updated\n");

    let err = rollback(&FsRollback, meta(), &path, "DEAD")
        .await
        .unwrap_err();

    assert!(err.contains("DEAD"), "error must mention the bad hash");
    assert!(err.contains(&h), "error must list known hashes");
    rm(&path);
}

#[tokio::test]
async fn rollback_error_already_at_requested_version() {
    let path = tmp("already_current_tool");
    write_file(&path, "v1\n");
    let h = record(&path, "v1\n").unwrap();
    write_file(&path, "v2\n");
    let _ = record(&path, "v2\n");

    // Restore to v1 first
    rollback(&FsRollback, meta(), &path, &h).await.unwrap();

    // Restore again — disk already has v1
    let err = rollback(&FsRollback, meta(), &path, &h).await.unwrap_err();

    assert!(
        err.contains("already at version"),
        "must error when file is already at the target, got: {err}"
    );
    rm(&path);
}

#[tokio::test]
async fn rollback_error_no_previous_when_only_one_version() {
    let path = tmp("single_version_tool");
    write_file(&path, "only version\n");
    let _ = record(&path, "only version\n");

    let err = rollback(&FsRollback, meta(), &path, "").await.unwrap_err();

    assert!(
        err.contains("no previous version"),
        "must error when at the oldest version, got: {err}"
    );
    rm(&path);
}
