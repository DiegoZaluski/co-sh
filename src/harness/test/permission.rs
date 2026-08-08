use std::path::Path;

use super::super::*;
use crate::harness::core::Mode;

// Mode::Yolo

#[test]
fn yolo_mode_allows_everything() {
    let args = serde_json::json!({ "command": "rm -rf /" });
    let result = check_tool_permission("bash_run", &args, Mode::Yolo, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn yolo_mode_allows_fs_outside_root() {
    let args = serde_json::json!({
        "targets": [{ "path": "/etc/passwd" }]
    });
    let result = check_tool_permission("fs_read", &args, Mode::Yolo, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

// Mode::Ask

#[test]
fn ask_mode_denies_bash() {
    let args = serde_json::json!({ "command": "ls" });
    let result = check_tool_permission("bash_run", &args, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn ask_mode_denies_fs_edit() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs", "file_hash": "abcd", "ops": "replace 1..1:\n+fn main() {}" }]
    });
    let result = check_tool_permission("fs_edit", &args, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn ask_mode_denies_fs_rollback() {
    let args = serde_json::json!({ "path": "src/main.rs", "hash": "abcd" });
    let result = check_tool_permission("fs_rollback", &args, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn ask_mode_fs_read_outside_cwd_needs_approval() {
    // fs_read targeting an absolute path outside root → NeedsApproval
    let tmp_root = std::env::temp_dir().join("cosh_test_ask_fs_root");
    let outside = std::env::temp_dir().join("cosh_test_ask_fs_outside.txt");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::write(&outside, b"test");

    let args = serde_json::json!({
        "targets": [{ "path": outside.to_str().unwrap() }]
    });
    let result = check_tool_permission("fs_read", &args, Mode::Ask, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_file(&outside);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "fs_read outside cwd should need approval even in Ask mode, got {result:?}"
    );
}

#[test]
fn ask_mode_fs_read_inside_cwd_is_allowed() {
    // fs_read with relative path inside cwd → Allowed (PathGuard handles it)
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Ask,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "fs_read inside cwd should be allowed in Ask mode, got {result:?}"
    );
}

#[test]
fn ask_mode_find_glob_outside_cwd_needs_approval() {
    // find_glob with absolute path outside root → NeedsApproval
    let tmp_root = std::env::temp_dir().join("cosh_test_ask_fg_root");
    let outside = std::env::temp_dir().join("cosh_test_ask_fg_outside");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::create_dir_all(&outside);

    let args = serde_json::json!({
        "pattern": "*.rs",
        "path": outside.to_str().unwrap()
    });
    let result = check_tool_permission("find_glob", &args, Mode::Ask, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&outside);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "find_glob outside cwd should need approval in Ask mode, got {result:?}"
    );
}

#[test]
fn ask_mode_skills_read_is_allowed() {
    let args = serde_json::json!({ "name": "my-skill" });
    let result = check_tool_permission("skills_read", &args, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn ask_mode_web_tools_are_allowed() {
    let args = serde_json::json!({ "url": "https://example.com" });
    let result = check_tool_permission("web_fetch", &args, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

// Mode::Build — tools that always need approval

#[test]
fn build_bash_needs_approval() {
    let args = serde_json::json!({ "command": "ls -la" });
    let result = check_tool_permission("bash_run", &args, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "bash_run");
        assert_eq!(req.args, "ls -la");
    }
}

#[test]
fn build_fs_edit_always_needs_approval() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs", "file_hash": "abcd", "ops": "replace 1..1:\n+fn main() {}" }]
    });
    let result = check_tool_permission("fs_edit", &args, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "fs_edit should always need approval, got {result:?}"
    );
}

#[test]
fn build_fs_write_always_needs_approval() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs", "text": "fn main() {}" }]
    });
    let result = check_tool_permission("fs_write", &args, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "fs_write should always need approval, got {result:?}"
    );
}

#[test]
fn build_fs_rollback_always_needs_approval() {
    let args = serde_json::json!({ "path": "src/main.rs", "hash": "abcd" });
    let result = check_tool_permission("fs_rollback", &args, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "fs_rollback should always need approval, got {result:?}"
    );
}

#[test]
fn build_subagent_needs_approval() {
    let args = serde_json::json!({ "agent": "claude" });
    let result = check_tool_permission("subagent_call", &args, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
}

// Mode::Build — tools that need approval only outside cwd

#[test]
fn build_fs_read_outside_cwd_needs_approval() {
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_fs_root");
    let outside_file = std::env::temp_dir().join("cosh_test_perm_fs_outside.txt");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::write(&outside_file, b"test");

    let args = serde_json::json!({
        "targets": [{ "path": outside_file.to_str().unwrap() }]
    });
    let result = check_tool_permission("fs_read", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_file(&outside_file);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "fs_read outside cwd should need approval, got {result:?}"
    );
}

#[test]
fn build_fs_read_inside_cwd_is_allowed() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Build,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "fs_read inside cwd should be allowed, got {result:?}"
    );
}

#[test]
fn build_find_glob_outside_cwd_needs_approval() {
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_fg_root");
    let outside_dir = std::env::temp_dir().join("cosh_test_perm_fg_outside");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::create_dir_all(&outside_dir);

    let args = serde_json::json!({
        "pattern": "*.rs",
        "path": outside_dir.to_str().unwrap()
    });
    let result = check_tool_permission("find_glob", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&outside_dir);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "find_glob outside cwd should need approval, got {result:?}"
    );
}

#[test]
fn build_find_glob_inside_cwd_is_allowed() {
    let args = serde_json::json!({
        "pattern": "*.rs",
        "path": "src"
    });
    let result = check_tool_permission(
        "find_glob",
        &args,
        Mode::Build,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "find_glob inside cwd should be allowed, got {result:?}"
    );
}

#[test]
fn build_find_grep_outside_cwd_needs_approval() {
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_gr_root");
    let outside_dir = std::env::temp_dir().join("cosh_test_perm_gr_outside");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::create_dir_all(&outside_dir);

    let args = serde_json::json!({
        "pattern": "fn main",
        "path": outside_dir.to_str().unwrap()
    });
    let result = check_tool_permission("find_grep", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&outside_dir);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "find_grep outside cwd should need approval, got {result:?}"
    );
}

#[test]
fn build_find_grep_inside_cwd_is_allowed() {
    let args = serde_json::json!({
        "pattern": "fn main",
        "path": "src"
    });
    let result = check_tool_permission(
        "find_grep",
        &args,
        Mode::Build,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "find_grep inside cwd should be allowed, got {result:?}"
    );
}

// The `paths` array (multi-target find_grep) must feed the same approval
// check as the single `path`: every target the tool opens is a real path the
// guard must see — one outside-root entry triggers approval even when `path`
// itself is absent. This is the guard-safe version of the oh-my-pi
// multi-target syntax: the guard never parses a bespoke string.
#[test]
fn build_find_grep_paths_array_outside_cwd_needs_approval() {
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_gr_paths_root");
    let outside_dir = std::env::temp_dir().join("cosh_test_perm_gr_paths_outside");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::create_dir_all(&outside_dir);

    let args = serde_json::json!({
        "pattern": "fn main",
        "paths": [
            "src",
            outside_dir.to_str().unwrap()
        ]
    });
    let result = check_tool_permission("find_grep", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&outside_dir);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "an outside-root entry in the paths array must trigger approval, got {result:?}"
    );
}

// The flip side: when every `paths` entry is inside the root, no approval is
// needed — the array does not change the guard's verdict for safe targets.
#[test]
fn build_find_grep_paths_array_inside_cwd_is_allowed() {
    let args = serde_json::json!({
        "pattern": "fn main",
        "paths": ["src", "tests"]
    });
    let result = check_tool_permission(
        "find_grep",
        &args,
        Mode::Build,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "all inside-root paths should be allowed, got {result:?}"
    );
}

// find_glob gained the same `paths` array (multi-target) as find_grep — the
// guard must see every target, and one outside-root entry triggers approval.
#[test]
fn build_find_glob_paths_array_outside_cwd_needs_approval() {
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_gl_paths_root");
    let outside_dir = std::env::temp_dir().join("cosh_test_perm_gl_paths_outside");
    let _ = std::fs::create_dir_all(&tmp_root);
    let _ = std::fs::create_dir_all(&outside_dir);

    let args = serde_json::json!({
        "pattern": "*.rs",
        "paths": [
            "src",
            outside_dir.to_str().unwrap()
        ]
    });
    let result = check_tool_permission("find_glob", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&outside_dir);

    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "an outside-root entry in the find_glob paths array must trigger approval, got {result:?}"
    );
}

#[test]
fn build_find_glob_paths_array_inside_cwd_is_allowed() {
    let args = serde_json::json!({
        "pattern": "*.rs",
        "paths": ["src", "tests"]
    });
    let result = check_tool_permission(
        "find_glob",
        &args,
        Mode::Build,
        Some(Path::new("/home/user/project")),
    );
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "all inside-root find_glob paths should be allowed, got {result:?}"
    );
}

// web tools — freely available

#[test]
fn build_web_fetch_is_allowed() {
    let args = serde_json::json!({ "url": "https://example.com" });
    let result = check_tool_permission("web_fetch", &args, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn build_web_search_is_allowed() {
    let args = serde_json::json!({ "query": "rust async" });
    let result = check_tool_permission("web_search", &args, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

// unknown tools (no check needed)

#[test]
fn unknown_tool_is_allowed() {
    let args = serde_json::json!({});
    let result = check_tool_permission("some_custom_mcp_tool", &args, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}
