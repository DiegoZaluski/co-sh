use std::path::Path;

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

/// Result of a pre-dispatch guardrail check.
#[derive(Debug)]
pub enum PermissionCheck {
    /// Tool is allowed to proceed without user interaction.
    Allowed,
    /// Tool needs user approval before execution.
    NeedsApproval(PermissionRequest),
    /// Tool is explicitly denied by mode restrictions.
    Denied(String),
}

/// Extract all paths from tool arguments for permission checking.
///
/// Supports the following argument shapes:
/// - `{ "targets": [{ "path": "..." }, ...] }` (fs_read, fs_write, fs_edit)
/// - `{ "path": "..." }` (fs_rollback)
/// - `{ "path": "..." }` / `{ "paths": [...] }` (find_glob, find_grep)
pub(crate) fn extract_paths_from_args(tool_name: &str, args: &Value) -> Vec<String> {
    match tool_name {
        "fs_read" | "fs_write" | "fs_edit" => {
            if let Some(targets) = args.get("targets").and_then(|v| v.as_array()) {
                targets
                    .iter()
                    .filter_map(|t| t.get("path").and_then(|p| p.as_str()))
                    .map(String::from)
                    .collect()
            } else {
                Vec::new()
            }
        }
        // find_glob / find_grep: the single `path` plus every entry of the
        // optional `paths` array — each target is a real path the tool opens,
        // so the approval check must see all of them.
        "find_glob" | "find_grep" => {
            let mut out: Vec<String> = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .into_iter()
                .collect();
            if let Some(list) = args.get("paths").and_then(|v| v.as_array()) {
                out.extend(list.iter().filter_map(|p| p.as_str().map(String::from)));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// True when `path` lives under the harness scratch directory
/// (`<OS temp>/cosh`), where truncated tool-output logs are written.
///
/// Scratch logs are ephemeral by definition — reading them back is part of
/// the truncation contract (the model is told to inspect them), so they must
/// never trigger the approval dialog. The PathGuard applies the same
/// exemption, keeping both layers consistent.
fn is_scratch_log_path(path: &Path) -> bool {
    let Ok(scratch) = std::env::temp_dir()
        .join(cosh_tools::util::path_guard::HARNESS_SCRATCH_DIR)
        .canonicalize()
    else {
        return false;
    };
    path.canonicalize()
        .ok()
        .is_some_and(|canon| canon.starts_with(&scratch))
}

/// Check whether any path in the list is absolute and outside the project root.
///
/// If an absolute path is detected **and** it does not start with the project
/// root (and is not under the harness scratch dir), the tool needs user
/// approval.
fn needs_path_approval(paths: &[String], project_root: Option<&Path>) -> Option<(String, String)> {
    for p in paths {
        let path = Path::new(p);
        if path.is_absolute() {
            // Ephemeral scratch logs are never approval-worthy.
            if is_scratch_log_path(path) {
                continue;
            }
            let inside_root = project_root
                .and_then(|root| root.canonicalize().ok())
                .is_some_and(|root_canon| {
                    path.canonicalize()
                        .ok()
                        .is_some_and(|p_canon| p_canon.starts_with(&root_canon))
                });

            if !inside_root {
                return Some((format!("access file: {p}"), format!("path: {p}")));
            }
        }
    }
    None
}

/// Check whether a tool call needs user permission before dispatch.
///
/// The harness acts as a guardrail — it checks **mode** (Ask/Build/Yolo)
/// and asks the user for approval on certain tool operations.
///
/// Tools that always need user approval (regardless of path):
/// - `bash_run`, `subagent_call` (execute external commands/code)
/// - `fs_edit`, `fs_rollback` (write/restore file operations)
///
/// Tools that need approval when targeting paths outside the project root:
/// - `fs_read` (read outside cwd)
/// - `find_glob`, `find_grep` (search outside cwd)
///   These also ask for approval in Ask mode (same as Build).
///
/// Path validation for all tools is delegated to
/// [`PathGuard`](cosh_tools::util::path_guard::PathGuard) via
/// [`resolve()`](cosh_tools::util::path_guard::PathGuard::resolve).
///
/// `project_root` is optional — when `None`, all absolute paths trigger
/// the permission dialog (defensive default).
#[must_use]
pub fn check_tool_permission(
    tool_name: &str,
    args: &Value,
    mode: Mode,
    project_root: Option<&Path>,
) -> PermissionCheck {
    // Yolo mode: everything is allowed without checks
    if mode == Mode::Yolo {
        return PermissionCheck::Allowed;
    }

    // Ask mode should only expose read-only tools.
    if mode == Mode::Ask && is_restricted_in_ask_mode(tool_name) {
        return PermissionCheck::Denied(format!("`{tool_name}` is not available in Ask mode"));
    }

    // ── Tools that always need user approval ──────────────────────
    // Checked BEFORE the path-based check so they never fall through
    // to Allowed for relative/inside-root paths.
    match tool_name {
        "bash_run" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "bash_run".to_string(),
                description: "execute a bash command".to_string(),
                args: command.to_string(),
            });
        }
        "fs_edit" | "fs_write" => {
            let paths = extract_paths_from_args(tool_name, args);
            let args_str = if paths.is_empty() {
                String::new()
            } else {
                paths.join(", ")
            };
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: tool_name.to_string(),
                description: format!("write file(s): {args_str}"),
                args: args_str,
            });
        }
        "fs_rollback" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "fs_rollback".to_string(),
                description: format!("restore file: {path}"),
                args: path,
            });
        }
        "subagent_call" => {
            // The merged tool routes to an INTERNAL agent when `agent` is
            // omitted/empty; show that in the dialog instead of "unknown".
            let agent = args
                .get("agent")
                .and_then(|v| v.as_str())
                .filter(|a| !a.trim().is_empty())
                .unwrap_or("internal");
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "subagent_call".to_string(),
                description: "call a sub-agent (external CLI or internal agent)".to_string(),
                args: format!("agent: {agent}"),
            });
        }
        _ => {}
    }

    // ── Path-based check: tools that need approval only outside cwd ──
    // fs_read, find_glob, find_grep: ask for confirmation when the
    // requested path is absolute and outside the project root.
    // When inside cwd (relative or absolute within root), let them
    // pass through — PathGuard handles the actual validation.
    let paths = extract_paths_from_args(tool_name, args);
    if !paths.is_empty() {
        if let Some((description, args_str)) = needs_path_approval(&paths, project_root) {
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: tool_name.to_string(),
                description,
                args: args_str,
            });
        }
        // All paths are relative or inside root — allow and let PathGuard
        // handle validation.
        return PermissionCheck::Allowed;
    }

    // Everything else passes through — path validation is handled by
    // PathGuard::resolve() inside the tool itself.
    PermissionCheck::Allowed
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
            | "subagent_call"
    )
}
