use std::path::{Path, PathBuf};

use super::super::*;
use crate::harness::core::Mode;

fn test_root() -> &'static Path {
    Path::new("/home/user/project")
}

fn no_allowlist() -> Option<&'static [PathBuf]> {
    None
}

fn no_blocklist() -> Option<&'static [PathBuf]> {
    None
}

// Mode::Yolo

#[test]
fn yolo_mode_allows_everything() {
    let args = serde_json::json!({ "command": "rm -rf /" });
    let result = check_tool_permission(
        "bash_run",
        &args,
        Mode::Yolo,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn yolo_mode_allows_fs_outside_root() {
    let args = serde_json::json!({
        "targets": [{ "path": "/etc/passwd" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Yolo,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

// Mode::Ask

#[test]
fn ask_mode_denies_write_tools() {
    let args = serde_json::json!({ "command": "ls" });
    let result = check_tool_permission(
        "bash_run",
        &args,
        Mode::Ask,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn ask_mode_allows_read_tools() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Ask,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

// Mode::Build

#[test]
fn build_allows_fs_inside_root() {
    let args = serde_json::json!({
        "targets": [{ "path": "src/main.rs" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn build_needs_approval_for_fs_outside_root() {
    let args = serde_json::json!({
        "targets": [{ "path": "/etc/passwd" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "fs_read");
        assert!(req.args.contains("/etc/passwd"));
    }
}

#[test]
fn build_denies_blocked_path() {
    let blocklist = [PathBuf::from("/etc")];
    let args = serde_json::json!({
        "targets": [{ "path": "/etc/shadow" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        Some(&blocklist as &[PathBuf]),
    );
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn build_allows_allowlisted_path_outside_root() {
    let allowlist = [PathBuf::from("/tmp/allowed.txt")];
    let args = serde_json::json!({
        "targets": [{ "path": "/tmp/allowed.txt" }]
    });
    let result = check_tool_permission(
        "fs_read",
        &args,
        Mode::Build,
        test_root(),
        Some(&allowlist as &[PathBuf]),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

// bash_run

#[test]
fn build_needs_approval_for_bash() {
    let args = serde_json::json!({ "command": "ls -la" });
    let result = check_tool_permission(
        "bash_run",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "bash_run");
        assert_eq!(req.args, "ls -la");
    }
}

// web tools

#[test]
fn build_needs_approval_for_web_fetch() {
    let args = serde_json::json!({ "url": "https://example.com" });
    let result = check_tool_permission(
        "web_fetch",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
}

#[test]
fn build_needs_approval_for_web_search() {
    let args = serde_json::json!({ "query": "rust async" });
    let result = check_tool_permission(
        "web_search",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
}

// single-path tools

#[test]
fn build_needs_approval_for_plan_todo_write() {
    let args = serde_json::json!({
        "path": "/tmp/TODO.md",
        "todos": []
    });
    let result = check_tool_permission(
        "plan_todo_write",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));
}

#[test]
fn build_allows_plan_todo_write_inside_root() {
    let args = serde_json::json!({
        "path": "TODO.md",
        "todos": []
    });
    let result = check_tool_permission(
        "plan_todo_write",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}

// unknown tools (no check needed)

#[test]
fn unknown_tool_is_allowed() {
    let args = serde_json::json!({});
    let result = check_tool_permission(
        "some_custom_mcp_tool",
        &args,
        Mode::Build,
        test_root(),
        no_allowlist(),
        no_blocklist(),
    );
    assert!(matches!(result, PermissionCheck::Allowed));
}
