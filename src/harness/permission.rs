use std::path::{Path, PathBuf};

use cosh_tools::util::guards::{GuardResult, validate_path};
use serde_json::Value;

use super::core::Mode;

/// Action the user can take in response to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Deny,
    AllowOnce,
}

/// Information about a tool call that needs user permission.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool: String,
    pub description: String,
    pub args: String,
}

/// Result of a pre-dispatch permission check.
#[derive(Debug)]
pub enum PermissionCheck {
    /// Tool is allowed to proceed without user interaction.
    Allowed,
    /// Tool needs user approval before execution.
    NeedsApproval(PermissionRequest),
    /// Tool is explicitly denied by blocklist or mode restrictions.
    Denied(String),
}

/// Check whether a tool call needs user permission before dispatch.
///
/// Returns `Allowed` when the tool operates within safe bounds,
/// `NeedsApproval` when the user should be prompted, and
/// `Denied` when the operation is blocked entirely.
#[must_use]
pub fn check_tool_permission(
    tool_name: &str,
    args: &Value,
    mode: Mode,
    root: &Path,
    allowlist: Option<&[PathBuf]>,
    blocklist: Option<&[PathBuf]>,
) -> PermissionCheck {
    // Yolo mode: everything is allowed without checks
    if mode == Mode::Yolo {
        return PermissionCheck::Allowed;
    }

    // Ask mode should only expose read-only tools.  If a write/execute
    // tool somehow gets called (e.g. via a compromised MCP server),
    // reject it immediately without prompting — these tools should not
    // even be visible to the model in Ask mode.
    if mode == Mode::Ask && is_restricted_in_ask_mode(tool_name) {
        return PermissionCheck::Denied(format!("`{tool_name}` is not available in Ask mode"));
    }

    match tool_name {
        "fs_read" => check_targets_paths(
            tool_name,
            args,
            "read files outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "fs_write" => check_targets_paths(
            tool_name,
            args,
            "write files outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "fs_edit" => check_targets_paths(
            tool_name,
            args,
            "edit files outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "fs_rollback" => check_single_path_field(
            tool_name,
            args,
            "roll back files outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "bash_run" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "bash_run".to_string(),
                description: "execute a bash command".to_string(),
                args: command.to_string(),
            })
        }
        "plan_todo_write" => check_single_path_field(
            tool_name,
            args,
            "write TODO file outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "plan_todo_edit" => check_single_path_field(
            tool_name,
            args,
            "edit TODO file outside the project root",
            root,
            allowlist,
            blocklist,
        ),
        "web_fetch" => PermissionCheck::NeedsApproval(PermissionRequest {
            tool: "web_fetch".to_string(),
            description: "fetch a URL from the web".to_string(),
            args: args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "web_search" => PermissionCheck::NeedsApproval(PermissionRequest {
            tool: "web_search".to_string(),
            description: "search the web".to_string(),
            args: format!(
                "query: {}",
                args.get("query").and_then(|v| v.as_str()).unwrap_or("")
            ),
        }),
        "subagent_call" => PermissionCheck::NeedsApproval(PermissionRequest {
            tool: "subagent_call".to_string(),
            description: "call an external AI sub-agent".to_string(),
            args: format!(
                "agent: {}",
                args.get("agent")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            ),
        }),
        _ => PermissionCheck::Allowed,
    }
}

/// Returns `true` for tools that are restricted in Ask mode (write/execute/external).
fn is_restricted_in_ask_mode(name: &str) -> bool {
    matches!(
        name,
        "fs_write"
            | "fs_edit"
            | "fs_rollback"
            | "bash_run"
            | "plan_todo_write"
            | "plan_todo_edit"
            | "web_fetch"
            | "web_search"
            | "subagent_call"
    )
}

/// Check paths in `targets[].path` from fs_read/write/edit argument format.
///
/// Returns `Denied` if any path hits the blocklist,
/// `NeedsApproval` if any path is outside the project root (not in allowlist),
/// `Allowed` if all paths are inside root or explicitly allowed.
fn check_targets_paths(
    tool_name: &str,
    args: &Value,
    description: &str,
    root: &Path,
    allowlist: Option<&[PathBuf]>,
    blocklist: Option<&[PathBuf]>,
) -> PermissionCheck {
    let Some(targets) = args.get("targets").and_then(|v| v.as_array()) else {
        return PermissionCheck::Allowed;
    };

    let mut blocked: Vec<String> = Vec::new();
    let mut outside: Vec<String> = Vec::new();

    for target in targets {
        let Some(path_str) = target.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        match validate_path(path_str, root, allowlist, blocklist) {
            GuardResult::Allowed(_) => {}
            GuardResult::Denied(msg) if msg.contains("blocklist") => {
                blocked.push(path_str.to_string());
            }
            GuardResult::Denied(_) => {
                outside.push(path_str.to_string());
            }
            GuardResult::Mismatch(msg) => {
                blocked.push(format!("{path_str}: {msg}"));
            }
        }
    }

    if !blocked.is_empty() {
        return PermissionCheck::Denied(format!(
            "paths blocked by blocklist: {}",
            blocked.join(", ")
        ));
    }

    if outside.is_empty() {
        return PermissionCheck::Allowed;
    }

    PermissionCheck::NeedsApproval(PermissionRequest {
        tool: tool_name.to_string(),
        description: description.to_string(),
        args: outside.join(", "),
    })
}

/// Check a single `path` field from tools like fs_rollback, plan_todo_write, etc.
fn check_single_path_field(
    tool_name: &str,
    args: &Value,
    description: &str,
    root: &Path,
    allowlist: Option<&[PathBuf]>,
    blocklist: Option<&[PathBuf]>,
) -> PermissionCheck {
    let Some(path_str) = args.get("path").and_then(|v| v.as_str()) else {
        return PermissionCheck::Allowed;
    };

    match validate_path(path_str, root, allowlist, blocklist) {
        GuardResult::Allowed(_) => PermissionCheck::Allowed,
        GuardResult::Denied(msg) if msg.contains("blocklist") => {
            PermissionCheck::Denied(format!("path blocked by blocklist: {path_str} ({msg})"))
        }
        GuardResult::Denied(_) => PermissionCheck::NeedsApproval(PermissionRequest {
            tool: tool_name.to_string(),
            description: description.to_string(),
            args: path_str.to_string(),
        }),
        GuardResult::Mismatch(msg) => {
            PermissionCheck::Denied(format!("path configuration error for {path_str}: {msg}"))
        }
    }
}
