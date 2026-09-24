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
/// - `{ "path": "..." }` — the advertised flat single-file form (fs_read,
///   fs_write, fs_edit, fs_edit_lines, fs_ast_edit, fs_rollback)
/// - `{ "targets": [{ "path": "..." }, ...] }` — the legacy batch form, kept
///   for single-element compatibility (fs_read, fs_write, fs_edit,
///   fs_edit_lines, fs_ast_edit)
/// - `{ "path": "..." }` / `{ "paths": [...] }` (find_glob, find_grep)
pub(crate) fn extract_paths_from_args(tool_name: &str, args: &Value) -> Vec<String> {
    match tool_name {
        "fs_read" | "fs_write" | "fs_edit" | "fs_edit_lines" | "fs_ast_edit" => {
            if let Some(targets) = args.get("targets").and_then(|v| v.as_array()) {
                targets
                    .iter()
                    .filter_map(|t| t.get("path").and_then(|p| p.as_str()))
                    .map(String::from)
                    .collect()
            } else {
                // The advertised flat single-file form: a flat {path, ...}.
                args.get("path")
                    .and_then(|p| p.as_str())
                    .map(String::from)
                    .into_iter()
                    .collect()
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
/// - `fs_edit`, `fs_edit_lines`, `fs_ast_edit`, `fs_rollback` (write/restore file
///   operations)
/// - `computer_act`, `computer_control` (synthetic input acts on the
///   whole desktop — outside any project-root sandbox)
///
/// Tree-grounded computer tools with a coordinate form (`computer_screenshot`,
/// `computer_control`):
/// the COORDINATE form is outright DENIED in Build mode — the Build approval
/// dialog hands focus to the TUI and the user may move the pointer while
/// answering, so any coordinate measured before the dialog can be stale.
/// The tree-grounded form of each (annotate=true capture; app/pid + selector
/// pointer targets) is allowed — it resolves from the a11y tree at dispatch
/// time, so no coordinate can go stale. Coordinate forms are fully available
/// in Yolo/Command mode, where dispatches run without an interactive dialog.
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

    // Command mode: the USER typed the command directly — the TUI is a plain
    // terminal and every dispatch is user-initiated. No dialog can be shown
    // (there is no agent loop asking); treat everything as pre-approved,
    // exactly like Yolo.
    if mode == Mode::Command {
        return PermissionCheck::Allowed;
    }

    // Ask mode should only expose read-only tools.
    if mode == Mode::Ask && is_restricted_in_ask_mode(tool_name) {
        return PermissionCheck::Denied(format!("`{tool_name}` is not available in Ask mode"));
    }

    // Position-dependent tools are hidden from the model in Build mode
    // (defense in depth): the Build approval dialog hands focus to the TUI
    // and the user may move the pointer while answering, so any coordinate
    // measured before the dialog can be stale by the time the action runs.
    // Deny outright instead of asking — an approval would re-create the
    // exact pointer-drift problem this guardrail exists to prevent.
    //
    // computer_screenshot is the exception: with `annotate: true` it is
    // selector-grounded (the model acts via computer_act with the legend's
    // selectors, never via pixels), so no coordinate can go stale — Build
    // allows exactly that form and denies the plain capture, which hands
    // the model pixel coordinates.
    if mode == Mode::Build {
        // Pointer: the element form (`app`/`pid` + `selector`) resolves the
        // point from the a11y tree at dispatch time — no coordinate can go
        // stale while the approval dialog is open, so it may ASK like any
        // other synthetic-input tool. The coordinate form (`x`/`y`, incl.
        // coordinate drag endpoints) is position-dependent: deny outright —
        // an approval would re-create the exact pointer-drift problem this
        // guardrail exists to prevent.
        // computer_control: the chain is checked AS A WHOLE before any
        // decision. A coordinate step ANYWHERE in it is position-dependent:
        // deny outright — an approval would re-create the exact
        // pointer-drift problem this guardrail exists to prevent. A chain
        // containing an element step may ask like any synthetic-input tool:
        // its real click moves OS keyboard focus AFTER the user answers the
        // dialog, which is exactly what makes the chained typing land on
        // the target. A chain that types (key/text) but never clicks an
        // element would send the text into whatever holds focus after the
        // dialog — the TUI input box (observed in the field) — so it is
        // denied with guidance toward the click-first pattern.
        if tool_name == "computer_control" {
            let mut step = Some(args);
            let mut index = 1usize;
            let mut element_seen = false;
            while let Some(s) = step {
                let is_keyboard = s.get("key").is_some() || s.get("text").is_some();
                if !is_keyboard
                    && (s.get("x").is_some()
                        || s.get("y").is_some()
                        || s.get("x2").is_some()
                        || s.get("y2").is_some())
                {
                    return PermissionCheck::Denied(format!(
                        "`computer_control` step {index} uses pixel coordinates: coordinates go \
                         stale when the Build approval dialog moves the pointer — target the \
                         element instead (`app`/`pid`/`surface` + `selector`), or switch to \
                         Yolo/Command mode"
                    ));
                }
                // ORDER-SENSITIVE (review finding): a keyboard step only
                // lands on the target when an EARLIER step's element click
                // has already moved OS keyboard focus. Typing before any
                // element step would reach whatever holds focus when
                // dispatch starts — the TUI input box after the approval
                // dialog (observed in the field) — so it is denied wherever
                // in the chain it appears.
                if is_keyboard && !element_seen {
                    return PermissionCheck::Denied(
                        "`computer_control` types into whatever element holds keyboard focus, \
                         and no earlier step in the chain clicks an element to set focus — \
                         the text would land in the cosh input box. Start with a click on \
                         the target element (`app`/`pid`/`surface` + `selector`) and chain \
                         the typing via `then`, or switch to Yolo/Command mode"
                            .to_string(),
                    );
                }
                element_seen |= s.get("selector").is_some();
                step = s.get("then");
                index += 1;
            }
            if element_seen {
                let app = args
                    .get("app")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        args.get("pid")
                            .and_then(Value::as_u64)
                            .map(|p| p.to_string())
                    })
                    .or_else(|| {
                        args.get("surface")
                            .and_then(Value::as_str)
                            .map(|s| format!("surface {s}"))
                    })
                    .unwrap_or_else(|| "?".to_string());
                return PermissionCheck::NeedsApproval(PermissionRequest {
                    tool: "computer_control".to_string(),
                    description: format!(
                        "pointer/keyboard pipeline on a desktop UI element ({app})"
                    ),
                    args: args
                        .get("selector")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                });
            }
            // Targetless pointer steps only (e.g. a lone `up`): still
            // synthetic desktop input — ask like any other.
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "computer_control".to_string(),
                description: "synthetic pointer/keyboard input on the desktop".to_string(),
                args: String::new(),
            });
        }
        // computer_act: the chain is checked AS A WHOLE, same policy shape
        // as computer_control. A chain whose keyboard steps all come AFTER
        // an element (semantic) step may ask like any synthetic-input tool
        // — the element action moves OS keyboard focus BEFORE the typing
        // runs, which is what makes the chained typing land on the target.
        // A keyboard step with NO earlier element step (regardless of what
        // comes later) would send the text into whatever holds focus after
        // the dialog — the TUI input box (observed in the field) — so it
        // is denied with guidance toward the element-first pattern (a
        // semantic press is NOT a reliable focus mover on every toolkit;
        // only a real click is).
        if tool_name == "computer_act" {
            let mut step = Some(args);
            let mut element_seen = false;
            while let Some(s) = step {
                let is_keyboard = s.get("key").is_some() || s.get("text").is_some();
                if is_keyboard && !element_seen {
                    return PermissionCheck::Denied(
                        "`computer_act` types into whatever element holds keyboard focus, \
                         and no earlier step in the chain acts on an element to set focus — \
                         the text would land in the cosh input box. Start with an element \
                         step (`name`/`pid`/`surface` + `selector`) and chain the typing \
                         via `then`; if the typing still misses the field, click it with \
                         computer_control first (a real click is the one reliable focus \
                         mover), or switch to Yolo/Command mode"
                            .to_string(),
                    );
                }
                element_seen |= s.get("selector").is_some();
                step = s.get("then");
            }
            if element_seen {
                let app = args
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        args.get("pid")
                            .and_then(Value::as_u64)
                            .map(|p| p.to_string())
                    })
                    .or_else(|| {
                        args.get("surface")
                            .and_then(Value::as_str)
                            .map(|s| format!("surface {s}"))
                    })
                    .unwrap_or_else(|| "?".to_string());
                return PermissionCheck::NeedsApproval(PermissionRequest {
                    tool: "computer_act".to_string(),
                    description: format!(
                        "semantic/keyboard pipeline on a desktop UI element ({app})"
                    ),
                    args: args
                        .get("selector")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                });
            }
            // Wait-only (or otherwise targetless) chain: still synthetic
            // desktop input — ask like any other.
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "computer_act".to_string(),
                description: "synthetic input on the desktop (wait-only chain)".to_string(),
                args: String::new(),
            });
        }
        if tool_name == "computer_screenshot"
            && !args
                .get("annotate")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return PermissionCheck::Denied(
                "`computer_screenshot` as a plain capture hands the model pixel \
                 coordinates that go stale when the Build approval dialog moves the \
                 pointer — use annotate=true for a selector-grounded capture (boxes + \
                 legend of selectors), or switch to Yolo/Command mode"
                    .to_string(),
            );
        }
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
        "fs_edit" | "fs_edit_lines" | "fs_ast_edit" | "fs_write" => {
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
        // Synthetic desktop input: acts on whatever the OS currently has
        // focused/under the cursor — there is no project-root sandbox to
        // fall back on, so every call asks (like bash_run). In Build mode
        // the chain-walk above has already returned; this arm serves
        // Yolo/Command mode.
        "computer_act" => {
            let target = args.get("selector").and_then(|v| v.as_str()).unwrap_or("?");
            let app = if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                name.to_string()
            } else {
                args.get("pid")
                    .and_then(|v| v.as_u64())
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".to_string())
            };
            return PermissionCheck::NeedsApproval(PermissionRequest {
                tool: "computer_act".to_string(),
                description: format!("perform an action on a desktop UI element ({app})"),
                args: target.to_string(),
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
/// The synthetic-input computer tools are included: even though their schemas are
/// hidden in Ask mode, a call supplied despite that (stale handoff, hallucination)
/// must be outright DENIED — Ask mode is read-only, and an approval dialog would
/// defeat the mode's contract.
fn is_restricted_in_ask_mode(name: &str) -> bool {
    matches!(
        name,
        "fs_write"
            | "fs_edit"
            | "fs_edit_lines"
            | "fs_ast_edit"
            | "fs_rollback"
            | "bash_run"
            | "plan_todo_write"
            | "subagent_call"
            | "computer_act"
            | "computer_control"
    )
}
