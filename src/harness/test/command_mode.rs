//! Command-mode invariants: the user-typed shell command and its output are
//! recorded in the context manager but stay HIDDEN from the model, and the
//! permission layer pre-approves everything (the agent loop never runs).

use super::super::context::ContextItem;
use super::super::*;
use crate::harness::core::Mode;

// ── Permission guardrails ──────────────────────────────────────────

#[test]
fn command_mode_allows_bash_without_approval() {
    // In Build mode the same dispatch would need user approval; the Command
    // mode executor bypasses the dialog entirely (there is no agent loop to
    // ask), so every dispatch is user-initiated and pre-approved.
    let args = serde_json::json!({ "command": "rm -rf /tmp/scratch" });
    let result = check_tool_permission("bash_run", &args, Mode::Command, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn command_mode_allows_fs_outside_root() {
    let args = serde_json::json!({
        "targets": [{ "path": "/etc/passwd" }]
    });
    let result = check_tool_permission("fs_read", &args, Mode::Command, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

#[test]
fn command_mode_allows_writes() {
    let args = serde_json::json!({
        "path": "src/main.rs",
        "file_hash": "abcd",
        "text": "fn main() {}"
    });
    let result = check_tool_permission("fs_edit", &args, Mode::Command, None);
    assert!(matches!(result, PermissionCheck::Allowed));
}

// ── Context recording ──────────────────────────────────────────────

#[test]
fn user_command_is_recorded_hidden() {
    let mut ctx = ContextManager::new(10_000);
    let id = ctx.add_hidden_user_command("ls -la");
    assert!(
        ctx.save_state().hidden.contains(&id),
        "the command item must be hidden from birth"
    );
    let item = ctx.items_snapshot().pop().unwrap();
    assert!(matches!(item, ContextItem::UserCommand { content, .. } if content == "ls -la"));
    // Zero token cost: the budget does not move (display-only, like Error).
    let before = ctx.display_info().total_tokens;
    ctx.add_hidden_command_execution("cmd-user-exec", "echo hi", &"x".repeat(4_000));
    assert_eq!(ctx.display_info().total_tokens, before);
}

#[test]
fn command_execution_is_recorded_hidden() {
    let mut ctx = ContextManager::new(10_000);
    let (call_id, result_id) =
        ctx.add_hidden_command_execution("cmd-user-exec", "ls", "file-a\nfile-b");
    let hidden = ctx.save_state().hidden;
    assert!(hidden.contains(&call_id), "the call must be hidden");
    assert!(hidden.contains(&result_id), "the result must be hidden");
    // The pair is a structural bash_run call/result, persisted for the user.
    let items = ctx.items_snapshot();
    assert!(matches!(&items[0], ContextItem::ToolCall { name, call_id: cid, .. }
        if name == "bash_run" && cid == "cmd-user-exec"));
    assert!(matches!(
        &items[1],
        ContextItem::ToolResult { content, call_id, .. }
            if content == "file-a\nfile-b" && call_id == "cmd-user-exec"
    ));
}

#[test]
fn build_messages_never_contains_command_items() {
    let mut ctx = ContextManager::new(10_000);
    ctx.add_user("visible user prompt");
    ctx.add_hidden_user_command("cat /etc/hostname");
    ctx.add_hidden_command_execution("cmd-user-exec", "cat /etc/hostname", "myhost");
    ctx.add_assistant("visible answer", true);

    let messages = ctx.build_messages("");
    // Serialize the WHOLE wire payload: a ToolCall renders with native
    // `tool_calls` (content: None), so a content-only join would never see
    // the command string inside the call arguments.
    let joined: String = messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("visible user prompt"));
    assert!(joined.contains("visible answer"));
    assert!(
        !joined.contains("cat /etc/hostname"),
        "the hidden command must never reach the model (content OR tool_calls)"
    );
    assert!(
        !joined.contains("myhost"),
        "the hidden execution output must never reach the model"
    );
}

#[test]
fn command_items_survive_save_restore_as_hidden() {
    let mut ctx = ContextManager::new(10_000);
    ctx.add_user("visible prompt");
    ctx.add_hidden_user_command("echo hi");
    ctx.add_hidden_command_execution("cmd-user-exec", "echo hi", "hi\n");

    let state = ctx.save_state();
    let mut restored = ContextManager::new(10_000);
    restored.restore_state(&state);

    assert!(
        restored
            .items_snapshot()
            .iter()
            .any(|it| matches!(it, ContextItem::UserCommand { content, .. } if content == "echo hi")),
        "the UserCommand item persists to the session JSONL"
    );
    let messages = restored.build_messages("");
    assert!(
        messages
            .iter()
            .all(|m| !m.content.as_deref().unwrap_or_default().contains("echo hi")),
        "the restored hidden items stay out of the model view"
    );
}

#[test]
fn command_items_do_not_count_toward_the_budget() {
    let mut ctx = ContextManager::new(10_000);
    let before = ctx.display_info().total_tokens;
    ctx.add_hidden_user_command("echo hi");
    ctx.add_hidden_command_execution("cmd-user-exec", "echo hi", &"x".repeat(4_000));
    assert_eq!(
        ctx.display_info().total_tokens,
        before,
        "hidden command items are display-only: zero tokens"
    );
}
