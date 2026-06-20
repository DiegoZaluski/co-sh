//! Demonstrates path-traversal behavior across fs tools.
//!
//! The `read` tool has no path validation — any path is accepted.
//! The `write` / `edit` / `rollback` tools use the shared guard from
//! `util::guards::validate_path` which normalizes `..`/`.` components
//! before comparing against the root, blocklist, and allowlist.
//!
//! These tests confirm the guard correctly **rejects** traversal attempts
//! (the fix for the original lexical-bypass vulnerability).

use std::path::Path;

use super::super::edit::edit;
use super::super::read::read;
use super::super::rollback::rollback;
use super::super::types::{
    EditTarget, FsEdit, FsMetadata, FsRead, FsRollback, FsWrite, Target, TargetFile,
};
use super::super::write::write;
use cosh_sdk::hashline::format::compute_file_hash;

const PROJECT_ROOT: &str = "/home/inky/cosh";

// Read — no guard at all

#[tokio::test]
async fn read_absolute_path_outside_root_succeeds() {
    let results = read(
        &FsRead,
        vec![Target {
            path: "/etc/hostname",
            line: None,
            symbol: None,
        }],
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_none(),
        "read should succeed without warnings: {:?}",
        results[0].warnings
    );
    assert!(
        !results[0].content.is_empty(),
        "content should not be empty"
    );
}

#[tokio::test]
async fn read_traversal_relative_path_escapes_cwd() {
    let cwd = std::env::current_dir().expect("cwd is accessible");
    let up_count = cwd.components().count();
    let parents: String = std::iter::repeat("..")
        .take(up_count)
        .collect::<Vec<_>>()
        .join("/");
    let traversal = format!("{parents}/etc/hostname");

    let results = read(
        &FsRead,
        vec![Target {
            path: &traversal,
            line: None,
            symbol: None,
        }],
    )
    .await;
    assert!(!results.is_empty(), "read should return results");
    assert!(
        results[0].warnings.is_none(),
        "traversal read should succeed: {:?}",
        results[0].warnings
    );
    assert!(
        !results[0].content.is_empty(),
        "content should not be empty"
    );
}

// Write — guard now correctly rejects traversal

#[tokio::test]
async fn write_traversal_via_dotdot_is_denied() {
    let traversal_path = "/home/inky/cosh/../../../tmp/cosh_traversal_write.txt";
    let resolved = "/tmp/cosh_traversal_write.txt";
    let _ = std::fs::remove_file(resolved);

    let metadata = FsMetadata {
        root: Path::new(PROJECT_ROOT),
        write_path_allowlist: None,
        write_path_blocklist: None,
    };

    let result = write(
        &FsWrite,
        metadata,
        vec![TargetFile {
            path: traversal_path,
            text: "TRAVERSAL_WRITE",
        }],
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
        !Path::new(resolved).exists(),
        "traversal path must not write outside root"
    );
}

#[tokio::test]
async fn write_traversal_blocklist_respected_after_normalization() {
    let traversal_path = "/home/inky/cosh/../../../tmp/cosh_traversal_blocked.txt";
    let resolved = "/tmp/cosh_traversal_blocked.txt";
    let _ = std::fs::remove_file(resolved);

    let metadata = FsMetadata {
        root: Path::new(PROJECT_ROOT),
        write_path_allowlist: None,
        write_path_blocklist: Some(vec![Path::new("/tmp")]),
    };

    let result = write(
        &FsWrite,
        metadata,
        vec![TargetFile {
            path: traversal_path,
            text: "BLOCKLIST_BYPASS",
        }],
    )
    .await;
    assert!(result.is_ok(), "write should not fail at outer level");

    let results = result.unwrap();
    assert!(
        results[0].warnings.is_some(),
        "write should be denied by blocklist after normalization"
    );

    assert!(
        !Path::new(resolved).exists(),
        "file must not be written despite blocklist"
    );
}

// Edit — guard now correctly rejects traversal

#[tokio::test]
async fn edit_traversal_via_dotdot_resolves_inside_root_and_succeeds() {
    let real_path = "/home/inky/cosh/cosh_traversal_edit_target.txt";
    std::fs::write(real_path, "original\n").unwrap();
    let hash = compute_file_hash("original\n");

    let traversal_path = "/home/inky/cosh/../cosh/cosh_traversal_edit_target.txt";

    let result = edit(
        &FsEdit,
        FsMetadata {
            root: Path::new(PROJECT_ROOT),
            write_path_allowlist: None,
            write_path_blocklist: None,
        },
        vec![EditTarget {
            path: traversal_path,
            file_hash: &hash,
            ops: "replace 1..1:\n+EDITED",
        }],
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
        &FsEdit,
        FsMetadata {
            root: Path::new(PROJECT_ROOT),
            write_path_allowlist: None,
            write_path_blocklist: None,
        },
        vec![EditTarget {
            path: escape_path,
            file_hash: "",
            ops: "replace 1..1:\n+EDITED",
        }],
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
    let _ = cosh_sdk::rollback::record(traversal_path, "version1\n");
    std::fs::write(real_path, "version2\n").unwrap();
    let _ = cosh_sdk::rollback::record(traversal_path, "version2\n");

    let result = rollback(
        &FsRollback,
        FsMetadata {
            root: Path::new(PROJECT_ROOT),
            write_path_allowlist: None,
            write_path_blocklist: None,
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
            root: Path::new(PROJECT_ROOT),
            write_path_allowlist: None,
            write_path_blocklist: None,
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
