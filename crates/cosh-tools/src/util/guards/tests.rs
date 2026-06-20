use std::path::{Path, PathBuf};

use super::{GuardResult, normalize_path, validate_asset_path, validate_path};

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
fn validate_path_denies_blocked_path() {
    let root = Path::new("/home/user/project");
    let blocklist = [Path::new("/home/user/project/secret")];
    let result = validate_path(
        "/home/user/project/secret/keys.txt",
        root,
        None,
        Some(&blocklist),
    );
    assert!(matches!(result, GuardResult::Denied(_)));
}

#[test]
fn validate_path_allows_allowlisted_path_outside_root() {
    let root = Path::new("/home/user/project");
    let allowlist = [Path::new("/tmp/allowed")];
    let result = validate_path("/tmp/allowed", root, Some(&allowlist), None);
    assert!(matches!(result, GuardResult::Allowed(_)));
}

#[test]
fn validate_path_mismatch_when_both_blocked_and_allowed() {
    let root = Path::new("/home/user/project");
    let allowlist = [Path::new("/tmp/conflict")];
    let blocklist = [Path::new("/tmp/conflict")];
    let result = validate_path("/tmp/conflict", root, Some(&allowlist), Some(&blocklist));
    assert!(matches!(result, GuardResult::Mismatch(_)));
}

#[test]
fn validate_path_blocklist_check_is_normalized() {
    // The blocklist entry and the path both use `..` but after normalization
    // they refer to the same location.
    let root = Path::new("/home/user/project");
    let blocklist = [Path::new("/tmp")];
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

// validate_asset_path

#[test]
fn validate_asset_path_rejects_absolute() {
    let base = Path::new("/some/dir");
    let result = validate_asset_path(base, "/etc/passwd");
    assert!(result.is_err(), "absolute paths should be rejected");
    assert!(result.unwrap_err().contains("absolute"));
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
        err.contains("asset not found") || err.contains("cannot canonicalize"),
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
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cosh_guards_test_{}", std::process::id()));
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
    // Create: base/asset -> ../outside (symlink)
    // The canonicalize + starts_with check should catch this.
    let dir = TempDir::new();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, "escaped").unwrap();
    let asset = dir.path().join("asset");
    std::fs::write(&asset, "real").unwrap();

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
