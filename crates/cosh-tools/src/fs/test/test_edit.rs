use super::super::edit::edit;
use super::super::types::{EditTarget, FsEdit, FsMetadata};
use cosh_sdk::hashline::format::compute_file_hash;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root: each test edits fixture files inside its own
/// directory, so tests never touch a developer's real project tree.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cosh_fs_edit_{label}_{id}_{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Path of a fixture file inside the root.
    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn make_metadata(root: &Path) -> FsMetadata {
    FsMetadata {
        root: root.to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

fn hash_file(path: &str) -> String {
    let content = std::fs::read_to_string(path).unwrap();
    compute_file_hash(&content)
}

#[tokio::test]
async fn edit_replaces_single_line_in_file() {
    let root = TempRoot::new("replace");
    let path = root.file("cosh_test_edit_replace.txt");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let file_hash = hash_file(&path);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: "replace 2..2:\n+REPLACED".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].first_changed_line, Some(2));

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "line1\nREPLACED\nline3\n");
}

#[tokio::test]
async fn edit_replaces_multi_line_range() {
    let root = TempRoot::new("range");
    let path = root.file("cosh_test_edit_range.txt");
    std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
    let file_hash = hash_file(&path);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: "replace 2..4:\n+X\n+Y".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "a\nX\nY\ne\n");
}

#[tokio::test]
async fn edit_inserts_before_and_after_anchor() {
    let root = TempRoot::new("anchor");
    let path = root.file("cosh_test_edit_insert.txt");
    std::fs::write(&path, "keep\nanchor\nkeep\n").unwrap();
    let file_hash = hash_file(&path);

    let ops = "insert before 2:\n+BEFORE\ninsert after 2:\n+AFTER";

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: ops.to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "keep\nBEFORE\nanchor\nAFTER\nkeep\n");
}

#[tokio::test]
async fn edit_inserts_at_head_and_tail() {
    let root = TempRoot::new("headtail");
    let path = root.file("cosh_test_edit_headtail.txt");
    std::fs::write(&path, "middle\n").unwrap();
    let file_hash = hash_file(&path);

    let ops = "insert head:\n+HEAD\ninsert tail:\n+TAIL";

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: ops.to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "HEAD\nmiddle\nTAIL\n");
}

#[tokio::test]
async fn edit_deletes_range_of_lines() {
    let root = TempRoot::new("delete");
    let path = root.file("cosh_test_edit_delete.txt");
    std::fs::write(&path, "keep1\ndelete1\ndelete2\nkeep2\n").unwrap();
    let file_hash = hash_file(&path);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: "delete 2..3".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "keep1\nkeep2\n");
}

#[tokio::test]
async fn edit_replaces_syntactic_block_in_rust_file() {
    let root = TempRoot::new("block");
    let path = root.file("cosh_test_edit_block.rs");
    std::fs::write(&path, "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
    let file_hash = hash_file(&path);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: "replace block 1:\n+fn main() {\n+    println!(\"hello\");\n+}".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "fn main() {\n    println!(\"hello\");\n}\n");
}

#[tokio::test]
async fn edit_processes_multiple_files_in_single_call() {
    let root = TempRoot::new("multifile");
    let path_a = root.file("cosh_test_edit_multi_a.txt");
    let path_b = root.file("cosh_test_edit_multi_b.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    let hash_a = hash_file(&path_a);
    let hash_b = hash_file(&path_b);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![
                EditTarget {
                    path: path_a.clone(),
                    file_hash: hash_a,
                    ops: "replace 1..1:\n+ALPHA".to_string(),
                },
                EditTarget {
                    path: path_b.clone(),
                    file_hash: hash_b,
                    ops: "replace 1..1:\n+BETA".to_string(),
                },
            ],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 2);

    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "BETA\n");
}

#[tokio::test]
async fn edit_returns_error_on_hash_mismatch() {
    let root = TempRoot::new("hashfail");
    let path = root.file("cosh_test_edit_hashfail.txt");
    std::fs::write(&path, "original\n").unwrap();

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: "BEEF".to_string(),
                ops: "replace 1..1:\n+changed".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not from this session") || msg.contains("edit failed"),
        "expected a hash-mismatch error, got: {msg}",
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
}

#[tokio::test]
async fn edit_refuses_auto_generated_file() {
    let root = TempRoot::new("generated");
    let path = root.file("cosh_test_edit_generated.rs");
    let original = "// Code generated by tool. DO NOT EDIT.\nfn main() {}\n";
    std::fs::write(&path, original).unwrap();
    let file_hash = hash_file(&path);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash,
                ops: "replace 1..1:\n+changed".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("auto-generated"),
        "expected an auto-generated refusal, got: {err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[tokio::test]
async fn edit_returns_error_when_file_not_found() {
    let root = TempRoot::new("notfound");
    let path = root.file("cosh_test_edit_nonexistent.txt");

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: "BEEF".to_string(),
                ops: "replace 1..1:\n+anything".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn edit_stops_at_failed_target_keeps_applied_and_skips_the_rest() {
    // Targets are applied in order; a failure in the middle aborts the batch.
    // Targets applied before the failure are kept, the failing target is the
    // root cause, and the remaining targets are skipped as a consequence —
    // never reported as independent failures.
    let root = TempRoot::new("cascade");
    let path_a = root.file("cosh_test_edit_cascade_a.txt");
    let path_b = root.file("cosh_test_edit_cascade_b.txt"); // fails: bad hash
    let path_c = root.file("cosh_test_edit_cascade_c.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    std::fs::write(&path_c, "gamma\n").unwrap();
    let hash_a = hash_file(&path_a);
    let hash_c = hash_file(&path_c);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![
                EditTarget {
                    path: path_a.clone(),
                    file_hash: hash_a,
                    ops: "replace 1..1:\n+ALPHA".to_string(),
                },
                EditTarget {
                    path: path_b.clone(),
                    file_hash: "BEEF".to_string(),
                    ops: "replace 1..1:\n+BETA".to_string(),
                },
                EditTarget {
                    path: path_c.clone(),
                    file_hash: hash_c,
                    ops: "replace 1..1:\n+GAMMA".to_string(),
                },
            ],
            dry_run: false,
        },
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Root cause: only the failing target is named as failed.
    assert_eq!(err.failed_path, path_b);
    assert!(!err.cause.is_empty());
    // Applied before the failure: kept as full results.
    assert_eq!(err.applied.len(), 1);
    assert_eq!(err.applied[0].path, path_a);
    // After the failure: skipped as a consequence, not failed.
    assert_eq!(err.skipped, vec![path_c.clone()]);

    let msg = err.to_string();
    assert!(msg.contains("edit failed for"), "got: {msg}");
    assert!(msg.contains("Applied before the failure"), "got: {msg}");
    assert!(msg.contains("Not applied (skipped because"), "got: {msg}");
    // The chain is named as consequence — never as a third independent failure.
    assert!(!msg.contains("GAMMA"), "got: {msg}");
    // The applied target's fresh hashline tag is surfaced so the model can
    // keep editing it without re-reading.
    let fresh_hash = compute_file_hash("ALPHA\n");
    let fresh_tag = format!("\u{b6}{path_a}#{fresh_hash}");
    assert!(
        msg.contains(&fresh_tag),
        "expected fresh tag {fresh_tag} in: {msg}"
    );

    // Disk state: A was applied, B and C untouched.
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "beta\n");
    assert_eq!(std::fs::read_to_string(&path_c).unwrap(), "gamma\n");
}

#[tokio::test]
async fn edit_first_target_failure_applies_nothing_and_skips_all_rest() {
    let root = TempRoot::new("firstfail");
    let path_a = root.file("cosh_test_edit_firstfail_a.txt");
    let path_b = root.file("cosh_test_edit_firstfail_b.txt");
    let path_c = root.file("cosh_test_edit_firstfail_c.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    std::fs::write(&path_c, "gamma\n").unwrap();
    let hash_b = hash_file(&path_b);
    let hash_c = hash_file(&path_c);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![
                EditTarget {
                    path: path_a.clone(),
                    file_hash: "BEEF".to_string(),
                    ops: "replace 1..1:\n+ALPHA".to_string(),
                },
                EditTarget {
                    path: path_b.clone(),
                    file_hash: hash_b,
                    ops: "replace 1..1:\n+BETA".to_string(),
                },
                EditTarget {
                    path: path_c.clone(),
                    file_hash: hash_c,
                    ops: "replace 1..1:\n+GAMMA".to_string(),
                },
            ],
            dry_run: false,
        },
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.failed_path, path_a);
    assert!(
        err.applied.is_empty(),
        "nothing may be applied before the first target"
    );
    assert_eq!(err.skipped, vec![path_b.clone(), path_c.clone()]);

    let msg = err.to_string();
    assert!(!msg.contains("Applied before the failure"), "got: {msg}");
    assert!(msg.contains("Not applied (skipped because"), "got: {msg}");

    // Nothing touched.
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "beta\n");
    assert_eq!(std::fs::read_to_string(&path_c).unwrap(), "gamma\n");
}

#[tokio::test]
async fn edit_last_target_failure_keeps_everything_before_it() {
    let root = TempRoot::new("lastfail");
    let path_a = root.file("cosh_test_edit_lastfail_a.txt");
    let path_b = root.file("cosh_test_edit_lastfail_b.txt");
    let path_c = root.file("cosh_test_edit_lastfail_c.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    std::fs::write(&path_c, "gamma\n").unwrap();
    let hash_a = hash_file(&path_a);
    let hash_b = hash_file(&path_b);

    let result = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![
                EditTarget {
                    path: path_a.clone(),
                    file_hash: hash_a,
                    ops: "replace 1..1:\n+ALPHA".to_string(),
                },
                EditTarget {
                    path: path_b.clone(),
                    file_hash: hash_b,
                    ops: "replace 1..1:\n+BETA".to_string(),
                },
                EditTarget {
                    path: path_c.clone(),
                    file_hash: "BEEF".to_string(),
                    ops: "replace 1..1:\n+GAMMA".to_string(),
                },
            ],
            dry_run: false,
        },
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.failed_path, path_c);
    assert_eq!(err.applied.len(), 2);
    assert!(err.skipped.is_empty(), "nothing follows the last target");

    let msg = err.to_string();
    assert!(msg.contains("Applied before the failure"), "got: {msg}");
    assert!(!msg.contains("Not applied"), "got: {msg}");

    // A and B were applied; C untouched.
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "BETA\n");
    assert_eq!(std::fs::read_to_string(&path_c).unwrap(), "gamma\n");
}
