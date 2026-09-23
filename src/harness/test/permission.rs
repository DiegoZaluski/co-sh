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
fn build_denies_coordinates_anywhere_in_the_chain_but_asks_for_element_chains() {
    // Coordinates depend on the pointer staying where it was when they
    // were measured — Build's approval dialog hands focus to the TUI and
    // the user may move the pointer while answering. Denied outright
    // (asking would re-create the pointer-drift problem), with guidance
    // toward the tree-grounded element form.
    let pointer = serde_json::json!({ "x": 100, "y": 200 });
    let result = check_tool_permission("computer_control", &pointer, Mode::Build, None);
    let denied = match result {
        PermissionCheck::Denied(msg) => msg,
        other => panic!("coordinate step must be denied in Build, got {other:?}"),
    };
    assert!(
        denied.contains("`app`/`pid`/`surface` + `selector`"),
        "the denial must point at the element form: {denied}"
    );

    // A coordinate drag is the same staleness problem (its start resolves
    // to pixels) — denied like any coordinate step.
    let drag = serde_json::json!({ "x": 10, "y": 10, "x2": 90, "y2": 90, "action": "drag" });
    let result = check_tool_permission("computer_control", &drag, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::Denied(_)),
        "coordinate drag must be denied in Build, got {result:?}"
    );

    // A coordinate step hidden DEEPER in a `then` chain is the same
    // position dependence — the guardrail walks the whole chain.
    let deeper = serde_json::json!({
        "app": "Firefox",
        "selector": "button[name='OK']",
        "then": { "x": 100, "y": 200 }
    });
    let result = check_tool_permission("computer_control", &deeper, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("step 2"),
                "the denial must name the failing step: {reason}"
            );
        }
        other => panic!("deeper coordinate step must be denied in Build, got {other:?}"),
    }

    // The element form resolves the point from the a11y tree at dispatch
    // time — no coordinate can go stale — so it ASKS like computer_act.
    let element = serde_json::json!({
        "app": "Firefox",
        "selector": "button[name='OK']",
        "action": "click"
    });
    let result = check_tool_permission("computer_control", &element, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "element-form step must ask in Build, got {result:?}"
    );
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "computer_control");
        assert!(req.args.contains("button[name='OK']"), "{}", req.args);
    }

    // Element-to-element drag: tree-grounded on both ends — asks too.
    let drag = serde_json::json!({
        "app": "Files",
        "selector": "list_item[name='report.pdf']",
        "to_selector": "group[name='Drop here']",
        "action": "drag"
    });
    let result = check_tool_permission("computer_control", &drag, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "element drag must ask in Build, got {result:?}"
    );
}

#[test]
fn yolo_mode_allows_coordinate_control() {
    let pointer = serde_json::json!({ "x": 100, "y": 200, "action": "click" });
    let result = check_tool_permission("computer_control", &pointer, Mode::Yolo, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn ask_mode_allows_screenshot_but_denies_control() {
    // Ask has no approval dialog, so the read-only screenshot stays
    // available; computer_control is synthetic input and stays restricted.
    let shot = serde_json::json!({});
    let result = check_tool_permission("computer_screenshot", &shot, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Allowed));

    let pointer = serde_json::json!({ "x": 100, "y": 200 });
    let result = check_tool_permission("computer_control", &pointer, Mode::Ask, None);
    assert!(matches!(result, PermissionCheck::Denied(_)));
}

#[test]
fn build_accessibility_tree_tools_still_need_approval() {
    // The tree tools keep their Build behavior: act asks, and the
    // read-only observation tools pass straight through.
    let act = serde_json::json!({ "name": "Safari", "selector": "button[name='OK']" });
    let result = check_tool_permission("computer_act", &act, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::NeedsApproval(_)));

    let apps = serde_json::json!({});
    let result = check_tool_permission("computer_apps", &apps, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::Allowed));

    let snapshot = serde_json::json!({ "name": "Safari" });
    let result = check_tool_permission("computer_snapshot", &snapshot, Mode::Build, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn computer_wait_is_allowed_in_every_mode() {
    // `computer_wait` is pure observation: it sends no input, moves no
    // pointer and opens no dialog, so a blocking wait is safe in every
    // mode — including Ask (the plan's allow-matrix decision for phase 5).
    // It must NOT appear in `is_restricted_in_ask_mode`.
    let wait = serde_json::json!({
        "name": "Safari",
        "selector": "progress_indicator",
        "state": "detached",
        "timeout_ms": 1000
    });
    for mode in [Mode::Ask, Mode::Build, Mode::Yolo] {
        let result = check_tool_permission("computer_wait", &wait, mode, None);
        assert!(
            matches!(result, PermissionCheck::Allowed),
            "computer_wait must be allowed in {mode:?}, got {result:?}"
        );
    }
}

#[test]
fn build_control_is_chain_aware_click_first_asks_keyboard_first_denied() {
    // The canonical click-first pipeline: a real click on the element
    // moves OS keyboard focus AFTER the user answers the dialog — exactly
    // what makes the chained typing land on the target — so the whole
    // chain asks once, like any synthetic-input tool.
    let click_first = serde_json::json!({
        "app": "Obsidian",
        "selector": "text_field[name='Untitled']",
        "action": "click",
        "then": { "text": "hello" }
    });
    let result = check_tool_permission("computer_control", &click_first, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "click-first pipeline must ask in Build, got {result:?}"
    );

    // A chain that STARTS with a keyboard step types into whatever holds
    // keyboard focus — and the approval dialog hands focus to the TUI, so
    // the text would land in the cosh input box (observed in the field).
    // Deny with guidance toward the click-first pattern.
    let keyboard_first = serde_json::json!({ "key": "enter" });
    let result = check_tool_permission("computer_control", &keyboard_first, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("click on the target element"),
                "denial must guide toward the click-first pattern: {reason}"
            );
        }
        other => panic!("keyboard-first chain must be denied in Build, got {other:?}"),
    }

    // The same keyboard step is allowed outright in Yolo (no approval
    // dialog exists there to steal focus — dispatch runs without an
    // interactive prompt, matching the coordinate form's Yolo behavior).
    let result = check_tool_permission("computer_control", &keyboard_first, Mode::Yolo, None);
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "keyboard step must be allowed in Yolo, got {result:?}"
    );

    // ORDER-SENSITIVE (review finding, same fix as computer_act): a
    // keyboard step BEFORE the element step is denied — the keystroke
    // would run before any click could set focus; a later element step
    // does not sanitize an earlier keystroke. The element-first mirror
    // passes the order check and asks.
    let type_then_click = serde_json::json!({
        "key": "enter",
        "then": { "app": "Obsidian", "selector": "text_field[name='Untitled']" }
    });
    let result = check_tool_permission("computer_control", &type_then_click, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("no earlier step"),
                "keyboard-before-element control chain must be denied: {reason}"
            );
        }
        other => panic!("keyboard-before-element control chain must be denied, got {other:?}"),
    }

    let click_then_type = serde_json::json!({
        "app": "Obsidian",
        "selector": "text_field[name='Untitled']",
        "then": { "key": "enter" }
    });
    let result = check_tool_permission("computer_control", &click_then_type, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "click-first control chain must ask in Build, got {result:?}"
    );
}

#[test]
fn build_act_is_chain_aware_element_first_asks_keyboard_first_denied() {
    // The canonical act pipeline (field-test pattern): a semantic action
    // on an element, then a wait, then typing into the field it created —
    // the whole chain asks once, like any synthetic-input tool.
    let element_first = serde_json::json!({
        "name": "Obsidian",
        "selector": "menu_item[name='Rename']",
        "then": {
            "wait": 600,
            "then": { "text": "cosh-test", "then": { "key": "enter" } }
        }
    });
    let result = check_tool_permission("computer_act", &element_first, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "element-first act pipeline must ask in Build, got {result:?}"
    );
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "computer_act");
        assert!(
            req.args.contains("menu_item[name='Rename']"),
            "{}",
            req.args
        );
    }

    // A chain that types but never touches an element sends the text into
    // whatever holds keyboard focus after the approval dialog — the cosh
    // input box (observed in the field). Deny with guidance toward the
    // element-first pattern (and the click fallback via computer_control).
    let keyboard_first = serde_json::json!({ "key": "enter" });
    let result = check_tool_permission("computer_act", &keyboard_first, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("element step"),
                "denial must guide toward the element-first pattern: {reason}"
            );
        }
        other => panic!("keyboard-first act chain must be denied in Build, got {other:?}"),
    }

    // ORDER-SENSITIVE (review finding): a keyboard step BEFORE the element
    // step is just as unsafe as a keyboard-only chain — the keystroke runs
    // before any element action could set focus. A later element step does
    // not sanitize an earlier keystroke.
    let type_then_element = serde_json::json!({
        "key": "enter",
        "then": { "name": "Obsidian", "selector": "text_field[name='Untitled']" }
    });
    let result = check_tool_permission("computer_act", &type_then_element, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("no earlier step"),
                "keyboard-before-element chain must be denied: {reason}"
            );
        }
        other => panic!("keyboard-before-element chain must be denied, got {other:?}"),
    }

    // The element-first mirror passes the order check and asks.
    let element_then_type = serde_json::json!({
        "name": "Obsidian",
        "selector": "text_field[name='Untitled']",
        "then": { "key": "enter" }
    });
    let result = check_tool_permission("computer_act", &element_then_type, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "element-first chain must ask in Build, got {result:?}"
    );

    // The same keyboard step is allowed outright in Yolo (no approval
    // dialog exists there to steal focus).
    let result = check_tool_permission("computer_act", &keyboard_first, Mode::Yolo, None);
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "keyboard step must be allowed in Yolo, got {result:?}"
    );

    // A wait-only chain carries no element to re-ground focus either; it
    // still asks like any synthetic-input tool (and is unrestricted in
    // Yolo).
    let wait_only = serde_json::json!({ "wait": 600, "then": { "wait": 400 } });
    let result = check_tool_permission("computer_act", &wait_only, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "wait-only act chain must ask in Build, got {result:?}"
    );
}

#[test]
fn build_act_surface_step_keeps_unchanged_approval_rules() {
    // Phase 6: `surface` is a THIRD targeting root on computer_act, not a
    // new tool — the guardrail matrix must be UNCHANGED. A surface step is
    // a semantic step (it carries `selector`), so it satisfies the
    // order-sensitive check exactly like a `name`/`pid` step: surface-first
    // chains ask in Build; a keyboard step BEFORE any selector step is
    // denied wherever a surface appears.
    let surface_first = serde_json::json!({
        "surface": "menu_bar",
        "selector": "menu_item[name='About']",
        "then": { "key": "escape" }
    });
    let result = check_tool_permission("computer_act", &surface_first, Mode::Build, None);
    assert!(
        matches!(result, PermissionCheck::NeedsApproval(_)),
        "surface-first act chain must ask in Build like any element chain, got {result:?}"
    );
    if let PermissionCheck::NeedsApproval(req) = result {
        assert_eq!(req.tool, "computer_act");
        assert!(
            req.args.contains("menu_item[name='About']"),
            "approval args must carry the surface step's selector: {}",
            req.args
        );
    }

    // A surface step LATER in the chain does not sanitize an earlier
    // keyboard step — same order-sensitive rule as the app path.
    let keyboard_then_surface = serde_json::json!({
        "key": "escape",
        "then": { "surface": "menu_bar", "selector": "menu_item[name='About']" }
    });
    let result = check_tool_permission("computer_act", &keyboard_then_surface, Mode::Build, None);
    match result {
        PermissionCheck::Denied(reason) => {
            assert!(
                reason.contains("no earlier step"),
                "keyboard-before-surface chain must be denied: {reason}"
            );
        }
        other => panic!("keyboard-before-surface chain must be denied, got {other:?}"),
    }

    // And the whole chain is allowed outright in Yolo (synthetic input,
    // unrestricted there like every other act chain).
    let result = check_tool_permission("computer_act", &surface_first, Mode::Yolo, None);
    assert!(
        matches!(result, PermissionCheck::Allowed),
        "surface-first act chain must be allowed in Yolo, got {result:?}"
    );
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

// The advertised single-file form (flat {path, ...}) must surface the same
// approval args as the batch form — the guard never parses a bespoke string.
#[test]
fn build_fs_edit_flat_form_carries_the_path_in_approval() {
    let args = serde_json::json!({
        "path": "src/main.rs",
        "file_hash": "abcd",
        "old_string": "a",
        "new_string": "b"
    });
    let result = check_tool_permission("fs_edit", &args, Mode::Build, None);
    assert!(
        matches!(&result, PermissionCheck::NeedsApproval(req) if req.args == "src/main.rs"),
        "flat fs_edit approval must name the edited path, got {result:?}"
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
fn build_fs_read_scratch_log_is_allowed_without_approval() {
    // Truncated-output logs live under <temp>/cosh — reading them back is part
    // of the truncation contract and must never trigger the approval dialog.
    let tmp_root = std::env::temp_dir().join("cosh_test_perm_scratch_root");
    let scratch_dir = std::env::temp_dir().join("cosh");
    let _ = std::fs::create_dir_all(&scratch_dir);
    let log = scratch_dir.join("cosh_test_perm_scratch.log");
    let _ = std::fs::write(&log, b"truncated middle content");

    let args = serde_json::json!({
        "targets": [{ "path": log.to_str().unwrap() }]
    });
    let result = check_tool_permission("fs_read", &args, Mode::Build, Some(tmp_root.as_path()));

    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_dir_all(&tmp_root);

    assert!(
        matches!(result, PermissionCheck::Allowed),
        "scratch log reads must be allowed without approval, got {result:?}"
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
