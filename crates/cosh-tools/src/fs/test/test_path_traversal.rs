//! Demonstrates path-traversal behavior across fs tools.
//!
//! All fs tools use the shared guard from `util::path_guard` which:
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

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root for each traversal test: the guard root is a per-test
/// temp directory, so the "outside root" targets land in a controlled,
/// isolated parent — never in a developer's real project tree.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cosh_fs_trav_{label}_{id}_{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Unique outside-target path in the OS temp dir (never under the root).
    fn outside(&self, label: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("cosh_trav_out_{label}_{id}_{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn meta(root: &TempRoot) -> FsMetadata {
    FsMetadata {
        root: root.path().to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

// Read guard now enforces path validation — absolute paths outside root
// are denied.

#[tokio::test]
async fn read_gap_absolute_path_no_guard() {
    let root = TempRoot::new("abs");
    let results = read(
        meta(&root),
        FsRead {
            targets: vec![Target {
                path: "/etc/hostname".to_string(),
                line: None,
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
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
    let root = TempRoot::new("dotdot");
    // Traverse above the temp root: guaranteed to escape any sane root.
    let traversal = format!(
        "{}/../cosh_trav_escape_target_{}",
        root.path().to_string_lossy(),
        std::process::id()
    );
    let results = read(
        meta(&root),
        FsRead {
            targets: vec![Target {
                path: traversal,
                line: None,
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
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
    let root = TempRoot::new("absdenied");
    let results = read(
        meta(&root),
        FsRead {
            targets: vec![Target {
                path: "/etc/hostname".to_string(),
                line: None,
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
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
    let root = TempRoot::new("relescape");
    let cwd = std::env::current_dir().expect("cwd is accessible");
    let up_count = cwd.components().count();
    let parents: String = std::iter::repeat_n("..", up_count)
        .collect::<Vec<_>>()
        .join("/");
    let traversal = format!("{parents}/etc/hostname");

    let results = read(
        meta(&root),
        FsRead {
            targets: vec![Target {
                path: traversal,
                line: None,
                symbol: None,
                line_range: None,

                offset: None,
                limit: None,
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
    let root = TempRoot::new("wdotdot");
    let resolved = root.outside("write");
    // A `..` chain deep enough to escape the temp root into the temp dir.
    let traversal_path = root
        .path()
        .join("..")
        .join(
            Path::new(&resolved)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .to_string_lossy()
        .into_owned();

    let _ = std::fs::remove_file(&resolved);

    let result = write(
        meta(&root),
        FsWrite {
            targets: vec![TargetFile {
                path: traversal_path,
                text: "TRAVERSAL_WRITE".to_string(),
                file_hash: None,
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
        !PathBuf::from(&resolved).exists(),
        "traversal path must not write outside root"
    );
}

#[tokio::test]
async fn write_traversal_blocklist_respected_after_normalization() {
    let root = TempRoot::new("wblock");
    let resolved = root.outside("blocked");
    // Same-shape traversal as the plain dotdot write: the guard normalizes
    // the path first, THEN applies the blocklist to the normalized form.
    let traversal_path = root
        .path()
        .join("..")
        .join(
            Path::new(&resolved)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .to_string_lossy()
        .into_owned();

    let _ = std::fs::remove_file(&resolved);

    let metadata = FsMetadata {
        root: root.path().to_path_buf(),
        allowlist: None,
        blocklist: Some(vec![std::env::temp_dir()]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: traversal_path,
                text: "BLOCKLIST_BYPASS".to_string(),
                file_hash: None,
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
        !PathBuf::from(&resolved).exists(),
        "file must not be written despite blocklist"
    );
}

// Canonicalize in fs_guard now catches symlink escapes.
// A symlink inside root pointing to an outside directory is denied.

#[tokio::test]
#[cfg(unix)]
async fn write_gap_symlink_escape() {
    let root = TempRoot::new("symlink_w");
    let outside_dir = root.outside("escape_dir");
    let symlink_path = root
        .path()
        .join("cosh_sandbox_escape_link")
        .to_string_lossy()
        .into_owned();
    let outside_file = format!("{outside_dir}/evil.txt");

    let _ = std::fs::remove_dir_all(&outside_dir);
    let _ = std::fs::remove_file(&symlink_path);

    std::fs::create_dir_all(&outside_dir).unwrap();
    std::os::unix::fs::symlink(&outside_dir, &symlink_path).unwrap();

    let symlink_target = format!("{symlink_path}/evil.txt");

    let result = write(
        meta(&root),
        FsWrite {
            targets: vec![TargetFile {
                path: symlink_target.clone(),
                text: "ESCAPED".to_string(),
                file_hash: None,
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

    let _ = std::fs::remove_dir_all(&outside_dir);
    let _ = std::fs::remove_file(&symlink_path);
}

// Canonicalize in fs_guard now catches symlink escapes for edit too.

#[tokio::test]
#[cfg(unix)]
async fn edit_gap_symlink_escape() {
    let root = TempRoot::new("symlink_e");
    let outside_dir = root.outside("edit_escape_dir");
    let symlink_path = root
        .path()
        .join("cosh_edit_escape_link")
        .to_string_lossy()
        .into_owned();
    let outside_file = format!("{outside_dir}/target.txt");

    let _ = std::fs::remove_dir_all(&outside_dir);
    let _ = std::fs::remove_file(&symlink_path);

    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(&outside_file, "original\n").unwrap();
    std::os::unix::fs::symlink(&outside_dir, &symlink_path).unwrap();

    let hash = compute_file_hash("original\n");

    let symlink_target = format!("{symlink_path}/target.txt");

    let result = edit(
        meta(&root),
        FsEdit {
            targets: vec![EditTarget {
                path: symlink_target,
                file_hash: hash,
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;

    assert!(
        result.is_err(),
        "edit should be denied via symlink after canonicalize: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("denied"),
        "error should mention denial: {err}"
    );

    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(
        content, "original\n",
        "file outside root must NOT be modified by edit"
    );

    let _ = std::fs::remove_dir_all(&outside_dir);
    let _ = std::fs::remove_file(&symlink_path);
}

// Edit — guard now correctly rejects traversal

#[tokio::test]
async fn edit_traversal_via_dotdot_resolves_inside_root_and_succeeds() {
    let root = TempRoot::new("editdotdot");
    let real_path = root
        .path()
        .join("cosh_traversal_edit_target.txt")
        .to_string_lossy()
        .into_owned();
    std::fs::write(&real_path, "original\n").unwrap();
    let hash = compute_file_hash("original\n");

    let traversal_path = root
        .path()
        .join("..")
        .join(
            root.path()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .join("cosh_traversal_edit_target.txt")
        .to_string_lossy()
        .into_owned();

    let result = edit(
        meta(&root),
        FsEdit {
            targets: vec![EditTarget {
                path: traversal_path,
                file_hash: hash,
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;

    assert!(
        result.is_ok(),
        "edit should succeed when normalized path is inside root: {result:?}"
    );

    let content = std::fs::read_to_string(&real_path).unwrap();
    assert_eq!(content, "EDITED\n");
}

#[tokio::test]
async fn edit_traversal_escape_via_dotdot_is_denied() {
    let root = TempRoot::new("editescape");
    let real_path = root
        .path()
        .join("cosh_traversal_edit_escape.txt")
        .to_string_lossy()
        .into_owned();
    std::fs::write(&real_path, "original\n").unwrap();

    let resolved = root.outside("edit_escape");
    let escape_path = root
        .path()
        .join("..")
        .join(
            Path::new(&resolved)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .to_string_lossy()
        .into_owned();

    let result = edit(
        meta(&root),
        FsEdit {
            targets: vec![EditTarget {
                path: escape_path,
                file_hash: "".to_string(),
                ops: "replace 1..1:\n+EDITED".to_string(),
            }],
            dry_run: false,
        },
    )
    .await;

    assert!(
        result.is_err(),
        "edit should be denied when normalized path escapes root: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("denied") || err.to_string().contains("outside"),
        "error should mention denial: {err}"
    );

    let _ = std::fs::remove_file(&resolved);
}

// Rollback — guard now correctly rejects traversal

#[tokio::test]
async fn rollback_traversal_via_dotdot_resolves_inside_root_and_succeeds() {
    let root = TempRoot::new("rbdotdot");
    let real_path = root
        .path()
        .join("cosh_traversal_rb_target.txt")
        .to_string_lossy()
        .into_owned();
    let traversal_path = root
        .path()
        .join("..")
        .join(
            root.path()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .join("cosh_traversal_rb_target.txt")
        .to_string_lossy()
        .into_owned();

    std::fs::write(&real_path, "version1\n").unwrap();
    let _ = cosh_sdk::rollback::record(&real_path, "version1\n");
    std::fs::write(&real_path, "version2\n").unwrap();
    let _ = cosh_sdk::rollback::record(&real_path, "version2\n");

    let result = rollback(&FsRollback, meta(&root), &traversal_path, "").await;

    assert!(
        result.is_ok(),
        "rollback should succeed when normalized path is inside root: {result:?}"
    );

    let content = std::fs::read_to_string(&real_path).unwrap();
    assert_eq!(content, "version1\n");
}

#[tokio::test]
async fn rollback_traversal_escape_via_dotdot_is_denied() {
    let root = TempRoot::new("rbescape");
    let real_path = root
        .path()
        .join("cosh_traversal_rb_escape.txt")
        .to_string_lossy()
        .into_owned();
    let resolved = root.outside("rb_escape");
    let escape_path = root
        .path()
        .join("..")
        .join(
            Path::new(&resolved)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .to_string_lossy()
        .into_owned();

    std::fs::write(&real_path, "version1\n").unwrap();
    let _ = cosh_sdk::rollback::record(&escape_path, "version1\n");

    let result = rollback(&FsRollback, meta(&root), &escape_path, "").await;

    assert!(
        result.is_err(),
        "rollback should be denied when normalized path escapes root: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("denied") || err.contains("outside"),
        "error should mention denial: {err}"
    );

    let _ = std::fs::remove_file(&resolved);
}
