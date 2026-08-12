use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    GuardResult, PathGuard, assert_editable_file, normalize_path, validate_asset_path,
    validate_path,
};

// normalize_path

#[test]
fn normalize_path_clean_absolute_is_unchanged() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("/home/user/project/src/main.rs"), root);
    assert_eq!(result, Path::new("/home/user/project/src/main.rs"));
}

#[test]
fn normalize_path_resolves_dot() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("/home/user/./project/./src/./main.rs"), root);
    assert_eq!(result, Path::new("/home/user/project/src/main.rs"));
}

#[test]
fn normalize_path_resolves_dotdot() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("/home/user/project/src/../lib.rs"), root);
    assert_eq!(result, Path::new("/home/user/project/lib.rs"));
}

#[test]
fn normalize_path_escape_above_root_preserves_parentdir() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("/home/user/project/../../../etc/passwd"), root);
    // Goes up above root — ParentDir preserved, so result won't match root
    assert_eq!(result, Path::new("/etc/passwd"));
}

#[test]
fn normalize_path_makes_relative_absolute() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("src/main.rs"), root);
    assert_eq!(result, Path::new("/home/user/project/src/main.rs"));
}

#[test]
fn normalize_path_relative_with_dotdot_escapes() {
    let root = Path::new("/home/user/project");
    let result = normalize_path(Path::new("../../../etc/passwd"), root);
    assert_eq!(result, Path::new("/etc/passwd"));
}

// validate_path

#[test]
fn validate_path_allows_path_inside_root() {
    let root = Path::new("/home/user/project");
    let result = validate_path("/home/user/project/src/main.rs", root, None, None);
    assert!(matches!(result, GuardResult::Allowed(_)));
}

#[test]
fn validate_path_denies_traversal_via_dotdot() {
    let root = Path::new("/home/user/project");
    let result = validate_path("/home/user/project/../../../etc/passwd", root, None, None);
    assert!(
        matches!(result, GuardResult::Denied(_)),
        "expected Denied, got {result:?}"
    );
}

#[test]
fn validate_path_denies_relative_traversal_via_dotdot() {
    let root = Path::new("/home/user/project");
    let result = validate_path("../../../etc/passwd", root, None, None);
    assert!(matches!(result, GuardResult::Denied(_)));
}

#[test]
fn validate_path_allows_harness_scratch_dir_outside_root() {
    // The harness scratch dir (<temp>/cosh, where truncated tool-output logs
    // live) is ephemeral — it must be readable without an explicit allowlist.
    let root = Path::new("/home/user/project");
    let scratch = std::env::temp_dir().join(super::HARNESS_SCRATCH_DIR);
    let log = scratch.join("0123456789abcdef.log");
    let result = validate_path(log.to_str().unwrap(), root, None, None);
    assert!(
        matches!(result, GuardResult::Allowed(_)),
        "scratch logs must be allowed outside root, got {result:?}"
    );
}

#[test]
fn validate_path_denies_scratch_dir_when_blocklisted() {
    // The blocklist keeps priority even over the scratch exemption.
    let root = Path::new("/home/user/project");
    let scratch = std::env::temp_dir().join(super::HARNESS_SCRATCH_DIR);
    let blocklist = [scratch.clone()];
    let log = scratch.join("deadbeef.log");
    let result = validate_path(log.to_str().unwrap(), root, None, Some(&blocklist));
    assert!(
        matches!(result, GuardResult::Denied(_)),
        "blocklisted scratch path must be denied, got {result:?}"
    );
}

#[test]
fn validate_path_denies_blocked_path() {
    let root = Path::new("/home/user/project").to_path_buf();
    let blocklist = [PathBuf::from("/home/user/project/secret")];
    let result = validate_path(
        "/home/user/project/secret/keys.txt",
        &root,
        None,
        Some(&blocklist),
    );
    assert!(matches!(result, GuardResult::Denied(_)));
}

#[test]
fn validate_path_allows_allowlisted_path_outside_root() {
    let root = Path::new("/home/user/project");
    let allowlist = [PathBuf::from("/tmp/allowed")];
    let result = validate_path("/tmp/allowed", root, Some(&allowlist), None);
    assert!(matches!(result, GuardResult::Allowed(_)));
}

#[test]
fn validate_path_mismatch_when_both_blocked_and_allowed() {
    let root = Path::new("/home/user/project");
    let allowlist = [PathBuf::from("/tmp/conflict")];
    let blocklist = [PathBuf::from("/tmp/conflict")];
    let result = validate_path("/tmp/conflict", root, Some(&allowlist), Some(&blocklist));
    assert!(matches!(result, GuardResult::Mismatch(_)));
}

#[test]
fn validate_path_blocklist_check_is_normalized() {
    // The blocklist entry and the path both use `..` but after normalization
    // they refer to the same location.
    let root = Path::new("/home/user/project");
    let blocklist = [PathBuf::from("/tmp")];
    // Path with `..` that resolves to /tmp/evil
    let result = validate_path(
        "/home/user/project/../../../tmp/evil",
        root,
        None,
        Some(&blocklist),
    );
    assert!(
        matches!(result, GuardResult::Denied(_)),
        "blocklist should catch normalized path: {result:?}"
    );
}

#[test]
fn validate_path_mismatch_message_content() {
    let root = Path::new("/home/user/project");
    let allowlist = [PathBuf::from("/tmp/conflict")];
    let blocklist = [PathBuf::from("/tmp/conflict")];
    let result = validate_path("/tmp/conflict", root, Some(&allowlist), Some(&blocklist));
    if let GuardResult::Mismatch(msg) = result {
        assert!(
            msg.contains("Security Alert"),
            "mismatch message should be a security alert, got: {msg}"
        );
        assert!(
            msg.contains("blocklist and allowlist"),
            "mismatch message should mention both lists, got: {msg}"
        );
    } else {
        panic!("expected Mismatch, got {result:?}");
    }
}

// validate_asset_path

#[test]
fn validate_asset_path_rejects_absolute() {
    let dir = TempDir::new();
    // Use a platform-appropriate absolute path
    let abs_path = if cfg!(windows) { "C:\\" } else { "/etc/passwd" };
    let result = validate_asset_path(dir.path(), abs_path);
    assert!(result.is_err(), "absolute paths should be rejected");
    assert!(result.unwrap_err().contains("absolute path"));
}

#[test]
fn validate_asset_path_rejects_dotdot() {
    let base = Path::new("/some/dir");
    let result = validate_asset_path(base, "../../../etc/passwd");
    assert!(result.is_err(), "paths with '..' should be rejected");
    assert!(result.unwrap_err().contains(".."));
}

#[test]
fn validate_asset_path_returns_not_found_for_nonexistent() {
    let dir = std::env::temp_dir().join("cosh_guard_test_vap_nonexistent");
    let _ = std::fs::create_dir_all(&dir);
    let result = validate_asset_path(&dir, "missing.txt");
    assert!(
        result.is_err(),
        "nonexistent asset should fail with not-found error"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("asset not found") || err.contains("could not resolve"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_asset_path_allows_valid_file() {
    let dir = std::env::temp_dir().join("cosh_guard_test_vap_valid");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("hello.txt");
    std::fs::write(&file_path, "hi").unwrap();

    let result = validate_asset_path(&dir, "hello.txt");
    assert!(
        result.is_ok(),
        "valid asset should be allowed: {:?}",
        result
    );
    assert_eq!(result.unwrap(), file_path.canonicalize().unwrap());
}

/// A TempDir that auto-cleans on drop.
/// Each instance uses a unique counter to avoid cross-test interference.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cosh_path_guard_test_{id}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn validate_asset_path_escape_via_symlink_rejected() {
    // Create: base/evil -> ../outside (symlink)
    // The canonicalize + starts_with check should catch this.
    let dir = TempDir::new();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, "escaped").unwrap();

    // Symlink from base/evil -> ../outside
    let evil = dir.path().join("evil");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", &evil).unwrap();
    }

    // Even with a symlink that points outside, canonicalize will resolve it
    // and starts_with will reject it.
    let result = validate_asset_path(dir.path(), "evil");
    assert!(
        result.is_err(),
        "escape via symlink should be rejected: {result:?}"
    );
}

// PathGuard::resolve
//
// PathGuard requires real filesystem paths for canonicalization.
// We use a TempDir (defined below) to create real directories.

#[test]
fn path_guard_resolves_path_inside_root() {
    let dir = TempDir::new();
    let sub = dir.path().join("src");
    std::fs::create_dir_all(&sub).unwrap();
    let file_path = sub.join("main.rs");
    std::fs::write(&file_path, "fn main() {}").unwrap();

    let root = dir.path();
    let guard = PathGuard::new(root, None, None);
    let result = guard.resolve(file_path.to_str().unwrap());
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

#[test]
fn path_guard_denies_path_outside_root() {
    let dir = TempDir::new();
    let root = dir.path();
    // Re-create the guard with a non-existent outside path
    let guard = PathGuard::new(root, None, None);
    // Use an absolute path that is clearly outside (parent of temp dir)
    let parent = root.parent().unwrap().join("outside.txt");
    let result = guard.resolve(parent.to_str().unwrap());
    assert!(
        result.is_err(),
        "expected Err for path outside root, got {result:?}"
    );
    assert!(
        result.unwrap_err().contains("permission denied"),
        "should contain standard error prefix"
    );
}

#[test]
fn path_guard_denies_path_in_blocklist() {
    let dir = TempDir::new();
    let secret = dir.path().join("secret");
    std::fs::create_dir_all(&secret).unwrap();
    let keys = secret.join("keys.txt");
    std::fs::write(&keys, "secret").unwrap();

    let blocklist = [secret.clone()];
    let guard = PathGuard::new(dir.path(), None, Some(&blocklist));
    let result = guard.resolve(keys.to_str().unwrap());
    assert!(
        result.is_err(),
        "expected Err for blocked path, got {result:?}"
    );
    assert!(result.unwrap_err().contains("permission denied"));
}

#[test]
fn path_guard_allows_allowlisted_path_outside_root() {
    let dir = TempDir::new();
    // Create a file in the temp dir's parent (outside the project root)
    // so that canonicalize() succeeds and validate_path can verify the
    // allowlist exception.
    let parent_dir = dir.path().parent().unwrap();
    let outside_file = parent_dir.join("cosh_guard_outside_allowed.txt");
    let _ = std::fs::write(&outside_file, "allowed content");

    let allowlist = [outside_file.clone()];
    let guard = PathGuard::new(dir.path(), Some(&allowlist), None);
    let result = guard.resolve(outside_file.to_str().unwrap());
    assert!(
        result.is_ok(),
        "expected Ok for allowlisted outside path, got {result:?}"
    );

    // Manual cleanup — TempDir only removes its own path.
    let _ = std::fs::remove_file(&outside_file);
}

#[test]
fn path_guard_mismatch_on_both_blocked_and_allowed() {
    let dir = TempDir::new();
    let conflict_file = dir.path().join("conflict.txt");
    std::fs::write(&conflict_file, "data").unwrap();

    // Same exact path in both allowlist and blocklist triggers Mismatch
    let allowlist = [conflict_file.clone()];
    let blocklist = [conflict_file.clone()];
    let guard = PathGuard::new(dir.path(), Some(&allowlist), Some(&blocklist));
    let result = guard.resolve(conflict_file.to_str().unwrap());
    assert!(result.is_err(), "expected Err for conflict, got {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.contains("Security Alert"),
        "expected Security Alert message, got: {err}"
    );
    assert!(
        err.contains("blocklist and allowlist"),
        "expected mention of both lists, got: {err}"
    );
}

#[test]
fn path_guard_denies_traversal_via_dotdot() {
    let dir = TempDir::new();
    let target = dir.path().join("../../../etc/passwd");
    let guard = PathGuard::new(dir.path(), None, None);
    let result = guard.resolve(target.to_str().unwrap());
    assert!(
        result.is_err(),
        "expected Err for traversal, got {result:?}"
    );
}

#[test]
fn path_guard_root_accessors() {
    let blocklist = [PathBuf::from("/tmp")];
    let guard = PathGuard::new(Path::new("/home/user/project"), None, Some(&blocklist));
    assert_eq!(guard.root(), &PathBuf::from("/home/user/project"));
    assert!(guard.allowlist().is_none());
    assert!(guard.blocklist().is_some());
    assert_eq!(guard.blocklist().unwrap(), &blocklist[..]);
}

#[test]
fn path_guard_add_allowlist_path() {
    let mut guard = PathGuard::new(Path::new("/home/user/project"), None, None);
    assert!(guard.allowlist().is_none());
    guard.add_allowlist_path(PathBuf::from("/tmp/allowed"));
    assert!(guard.allowlist().is_some());
    assert_eq!(guard.allowlist().unwrap(), &[PathBuf::from("/tmp/allowed")]);
    // Adding same path again is no-op
    guard.add_allowlist_path(PathBuf::from("/tmp/allowed"));
    assert_eq!(guard.allowlist().unwrap().len(), 1);
}

// assert_editable_file

#[test]
fn assert_editable_allows_nonexistent_file() {
    // Creating a new file is always allowed — nothing existing is overwritten.
    let dir = TempDir::new();
    let missing = dir.path().join("missing.txt");
    assert!(assert_editable_file(&missing).is_ok());
}

#[test]
fn assert_editable_rejects_go_generated_header() {
    let dir = TempDir::new();
    let f = dir.path().join("gen.pb.go");
    std::fs::write(
        &f,
        "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage x\n",
    )
    .unwrap();
    let err = assert_editable_file(&f).unwrap_err();
    assert!(
        err.contains("auto-generated") && err.contains("do not edit"),
        "expected an actionable auto-generated error, got: {err}"
    );
}

#[test]
fn assert_editable_rejects_typescript_generated_annotation() {
    let dir = TempDir::new();
    let f = dir.path().join("gen.ts");
    std::fs::write(&f, "// @generated by protoc-gen-es v1.2.3\n").unwrap();
    assert!(assert_editable_file(&f).is_err());
}

#[test]
fn assert_editable_rejects_plain_generated_variant() {
    let dir = TempDir::new();
    let f = dir.path().join("gen.py");
    std::fs::write(&f, "# This file was automatically generated.\n").unwrap();
    assert!(assert_editable_file(&f).is_err());
}

#[test]
fn assert_editable_allows_plain_file() {
    let dir = TempDir::new();
    let f = dir.path().join("plain.rs");
    std::fs::write(&f, "fn main() {}\n").unwrap();
    assert!(assert_editable_file(&f).is_ok());
}

#[test]
fn assert_editable_allows_marker_past_scan_window() {
    // The cheap check only scans the first few lines; a "DO NOT EDIT" deep in
    // the file body is not a generation header and passes.
    let dir = TempDir::new();
    let f = dir.path().join("deep.txt");
    let mut content = String::new();
    for i in 0..30 {
        content.push_str(&format!("line {i}\n"));
    }
    content.push_str("// DO NOT EDIT\n");
    std::fs::write(&f, &content).unwrap();
    assert!(assert_editable_file(&f).is_ok());
}

#[test]
fn assert_editable_allows_empty_file() {
    let dir = TempDir::new();
    let f = dir.path().join("empty.txt");
    std::fs::write(&f, "").unwrap();
    assert!(assert_editable_file(&f).is_ok());
}

#[test]
fn validate_asset_path_dotdot_after_symlink_rejected() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("target.txt"), "content").unwrap();

    // Symlink inside base that points to a sibling dir.
    let link = dir.path().join("link");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("sub", &link).unwrap();
    }
    // Try to escape via the symlinked dir using `..`
    // link/../../../etc/passwd — but validate_asset_path rejects `..` outright.
    let result = validate_asset_path(dir.path(), "link/../../../etc/passwd");
    assert!(
        result.is_err(),
        "path with '..' should be rejected even with symlinks"
    );
}
