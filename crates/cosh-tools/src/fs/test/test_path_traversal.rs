//! Demonstrates path-traversal behavior across fs tools.
//!
//! All fs tools use the shared guard from `util::guards` which:
//! 1. Normalizes `..`/`.` components lexically
//! 2. Compares against root, blocklist, and allowlist
//! 3. Canonicalizes paths to catch symlink escapes
//!
//! The `_gap` suffix tests assert corrected behavior for previously
//! unpatched vulnerabilities (closed by sandbox integrity fixes).

use std::path::PathBuf;

use super::super::edit::edit;
use super::super::read::read;
use super::super::rollback::rollback;
use super::super::types::{
    EditTarget, FsEdit, FsMetadata, FsRead, FsRollback, FsWrite, Target, TargetFile,
};
use super::super::write::write;
use cosh_sdk::hashline::format::compute_file_hash;

const PROJECT_ROOT: &str = "/home/inky/cosh";

fn meta() -> FsMetadata {
    FsMetadata {
        root: PathBuf::from(PROJECT_ROOT),
        allowlist: None,
        blocklist: None,
    }
}

// Read guard now enforces path validation — absolute paths outside root
// are denied.

#[tokio::test]
async fn read_gap_absolute_path_no_guard() {
    let results = read(
        meta(),
        FsRead {
            targets: vec![Target {
                path: "/etc/hostname".to_string(),
                line: None,
                symbol: None,
            }],
        },
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_some(),
        "guard should deny absolute path outside root: {:?}",
        results[0].warnings
    );
    let warning = results[0].warnings.as_ref().unwrap();
    assert!(
        warning.contains("denied"),
        "warning should mention denial: {warning}"
    );
}

// Read guard also rejects `..` path traversal.

#[tokio::test]
async fn read_gap_dotdot_traversal_no_guard() {
    let results = read(
        meta(),
        FsRead {
            targets: vec![Target {
                path: "/home/inky/cosh/../../../etc/hostname".to_string(),
                line: None,
                symbol: None,
            }],
        },
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_some(),
        "guard should deny `..` traversal: {:?}",
        results[0].warnings
    );
    let warning = results[0].warnings.as_ref().unwrap();
    assert!(
        warning.contains("denied"),
        "warning should mention denial: {warning}"
    );
}

#[tokio::test]
async fn read_absolute_path_outside_root_is_denied() {
    let results = read(
        meta(),
        FsRead {
            targets: vec![Target {
                path: "/etc/hostname".to_string(),
                line: None,
                symbol: None,
            }],
        },
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_some(),
        "read should be denied outside root: {:?}",
        results[0].warnings
    );
}

#[tokio::test]
async fn read_traversal_relative_path_escapes_denied() {
    let cwd = std::env::current_dir().expect("cwd is accessible");
    let up_count = cwd.components().count();
    let parents: String = std::iter::repeat("..")
        .take(up_count)
        .collect::<Vec<_>>()
        .join("/");
    let traversal = format!("{parents}/etc/hostname");

    let results = read(
        meta(),
        FsRead {
            targets: vec![Target {
                path: traversal,
                line: None,
                symbol: None,
            }],
        },
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_some(),
        "read should deny relative traversal: {:?}",
        results[0].warnings
    );
}

// Write — guard now correctly rejects traversal

#[tokio::test]
async fn write_traversal_via_dotdot_is_denied() {
    let traversal_path = "/home/inky/cosh/../../../tmp/cosh_traversal_write.txt";
    let resolved = "/tmp/cosh_traversal_write.txt";
    let _ = std::fs::remove_file(resolved);

    let _metadata = FsMetadata {
        root: PathBuf::from(PROJECT_ROOT),
        allowlist: None,
        blocklist: None,
    };

    let result = write(
        _metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: traversal_path.to_string(),
                text: "TRAVERSAL_WRITE".to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok(), "write should not fail at outer level");

    let results = result.unwrap();
    assert!(
        results[0].warnings.is_some(),
        "write should produce a warning denying the traversal: {:?}",
        results[0]
    );
    let warning = results[0].warnings.as_ref().unwrap();
    assert!(
        warning.contains("denied") || warning.contains("outside"),
        "warning should mention denial: {warning}"
    );

    assert!(
        !PathBuf::from(resolved).exists(),
        "traversal path must not write outside root"
    );
}

#[tokio::test]
async fn write_traversal_blocklist_respected_after_normalization() {
    let traversal_path = "/home/inky/cosh/../../../tmp/cosh_traversal_blocked.txt";
    let resolved = "/tmp/cosh_traversal_blocked.txt";
    let _ = std::fs::remove_file(resolved);

    let metadata = FsMetadata {
        root: PathBuf::from(PROJECT_ROOT),
        allowlist: None,
        blocklist: Some(vec![PathBuf::from("/tmp")]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: traversal_path.to_string(),
                text: "BLOCKLIST_BYPASS".to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok(), "write should not fail at outer level");

    let results = result.unwrap();
    assert!(
        results[0].warnings.is_some(),
        "write should be denied by blocklist after normalization"
    );

    assert!(
        !PathBuf::from(resolved).exists(),
        "file must not be written despite blocklist"
    );
}

// Canonicalize in fs_guard now catches symlink escapes.
// A symlink inside root pointing to an outside directory is denied.

#[tokio::test]
#[cfg(unix)]
async fn write_gap_symlink_escape() {
    let outside_dir = "/tmp/cosh_symlink_escape_target";
    let symlink_path = "/home/inky/cosh/cosh_sandbox_escape_link";
    let outside_file = format!("{outside_dir}/evil.txt");

    let _ = std::fs::remove_dir_all(outside_dir);
    let _ = std::fs::remove_file(&symlink_path);

    std::fs::create_dir_all(outside_dir).unwrap();
    let _ = std::fs::remove_file(&symlink_path);
    std::os::unix::fs::symlink(outside_dir, &symlink_path).unwrap();

    let metadata = FsMetadata {
        root: PathBuf::from(PROJECT_ROOT),
        allowlist: None,
        blocklist: None,
    };

    let symlink_target = format!("{symlink_path}/evil.txt");

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: symlink_target.clone(),
                text: "ESCAPED".to_string(),
            }],
        },
    )
    .await;

    assert!(result.is_ok(), "write should not fail at outer level");
    let results = result.unwrap();
    assert!(
        results[0].warnings.is_some(),
        "canonicalized guard should deny symlink escape: {:?}",
        results[0].warnings
    );
    let warning = results[0].warnings.as_ref().unwrap();
    assert!(
        warning.contains("denied"),
        "warning should mention denial: {warning}"
    );

    assert!(
        !PathBuf::from(&outside_file).exists(),
        "file must NOT exist — write was denied via symlink escape"
    );

    let _ = std::fs::remove_dir_all(outside_dir);
    let _ = std::fs::remove_file(&symlink_path);
}

// Canonicalize in fs_guard now catches symlink escapes for edit too.

#[tokio::test]
#[cfg(unix)]
async fn edit_gap_symlink_escape() {
    let outside_dir = "/tmp/cosh_symlink_edit_escape";
    let symlink_path = "/home/inky/cosh/cosh_edit_escape_link";
    let outside_file = format!("{outside_dir}/target.txt");

    let _ = std::fs::remove_dir_all(outside_dir);
    let _ = std::fs::remove_file(&symlink_path);

    std::fs::create_dir_all(outside_dir).unwrap();
    std::fs::write(&outside_file, "original\n").unwrap();
    std::os::unix::fs::symlink(outside_dir, &symlink_path).unwrap();

    let hash = compute_file_hash("original\n");

    let symlink_target = format!("{symlink_path}/target.txt");

    let result = edit(
        FsMetadata {
            root: PathBuf::from(PROJECT_ROOT),
            allowlist: None,
            blocklist: None,
        },
        FsEdit {
            targets: vec![EditTarget {
                path: symlink_target,
                file_hash: hash,
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
        },
    )
    .await;

    assert!(
        result.is_err(),
        "edit should be denied via symlink after canonicalize: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(err.contains("denied"), "error should mention denial: {err}");

    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(
        content, "original\n",
        "file outside root must NOT be modified by edit"
    );

    let _ = std::fs::remove_dir_all(outside_dir);
    let _ = std::fs::remove_file(&symlink_path);
}

// Edit — guard now correctly rejects traversal

#[tokio::test]
async fn edit_traversal_via_dotdot_resolves_inside_root_and_succeeds() {
    let real_path = "/home/inky/cosh/cosh_traversal_edit_target.txt";
    std::fs::write(real_path, "original\n").unwrap();
    let hash = compute_file_hash("original\n");

    let traversal_path = "/home/inky/cosh/../cosh/cosh_traversal_edit_target.txt";

    let result = edit(
        FsMetadata {
            root: PathBuf::from(PROJECT_ROOT),
            allowlist: None,
            blocklist: None,
        },
        FsEdit {
            targets: vec![EditTarget {
                path: traversal_path.to_string(),
                file_hash: hash,
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
        },
    )
    .await;

    assert!(
        result.is_ok(),
        "edit should succeed when normalized path is inside root: {result:?}"
    );

    let content = std::fs::read_to_string(real_path).unwrap();
    assert_eq!(content, "EDITED\n");

    let _ = std::fs::remove_file(real_path);
}

#[tokio::test]
async fn edit_traversal_escape_via_dotdot_is_denied() {
    let real_path = "/home/inky/cosh/cosh_traversal_edit_escape.txt";
    std::fs::write(real_path, "original\n").unwrap();

    let escape_path = "/home/inky/cosh/../../../tmp/cosh_traversal_edit_escape.txt";
    let resolved = "/tmp/cosh_traversal_edit_escape.txt";

    let result = edit(
        FsMetadata {
            root: PathBuf::from(PROJECT_ROOT),
            allowlist: None,
            blocklist: None,
        },
        FsEdit {
            targets: vec![EditTarget {
                path: escape_path.to_string(),
                file_hash: "".to_string(),
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
        },
    )
    .await;

    assert!(
        result.is_err(),
        "edit should be denied when normalized path escapes root: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("denied") || err.contains("outside"),
        "error should mention denial: {err}"
    );

    let _ = std::fs::remove_file(real_path);
    let _ = std::fs::remove_file(resolved);
}

// Rollback — guard now correctly rejects traversal

#[tokio::test]
async fn rollback_traversal_via_dotdot_resolves_inside_root_and_succeeds() {
    let real_path = "/home/inky/cosh/cosh_traversal_rb_target.txt";
    let traversal_path = "/home/inky/cosh/../cosh/cosh_traversal_rb_target.txt";

    std::fs::write(real_path, "version1\n").unwrap();
    let _ = cosh_sdk::rollback::record(real_path, "version1\n");
    std::fs::write(real_path, "version2\n").unwrap();
    let _ = cosh_sdk::rollback::record(real_path, "version2\n");

    let result = rollback(
        &FsRollback,
        FsMetadata {
            root: PathBuf::from(PROJECT_ROOT),
            allowlist: None,
            blocklist: None,
        },
        traversal_path,
        "",
    )
    .await;

    assert!(
        result.is_ok(),
        "rollback should succeed when normalized path is inside root: {result:?}"
    );

    let content = std::fs::read_to_string(real_path).unwrap();
    assert_eq!(content, "version1\n");

    let _ = std::fs::remove_file(real_path);
}

#[tokio::test]
async fn rollback_traversal_escape_via_dotdot_is_denied() {
    let real_path = "/home/inky/cosh/cosh_traversal_rb_escape.txt";
    let escape_path = "/home/inky/cosh/../../../tmp/cosh_traversal_rb_escape.txt";

    std::fs::write(real_path, "version1\n").unwrap();
    let _ = cosh_sdk::rollback::record(escape_path, "version1\n");

    let result = rollback(
        &FsRollback,
        FsMetadata {
            root: PathBuf::from(PROJECT_ROOT),
            allowlist: None,
            blocklist: None,
        },
        escape_path,
        "",
    )
    .await;

    assert!(
        result.is_err(),
        "rollback should be denied when normalized path escapes root: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("denied") || err.contains("outside"),
        "error should mention denial: {err}"
    );

    let _ = std::fs::remove_file(real_path);
}
