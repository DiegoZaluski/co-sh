//! Synthesis of typed [`SubagentEvent`]s for the INTERNAL sub-agent.
//!
//! The external ACP harnesses emit their activity natively: the `session/update`
//! stream is mapped into [`SubagentEvent`]s by [`super::events`]. The internal
//! sub-agent is a nested cosh harness and speaks no ACP — its loop emits plain
//! [`crate::harness`-level] events (tool calls, results, context info). This
//! module bridges that gap: it turns the nested loop's event stream into the
//! SAME typed events the TUI sub-agent box already renders, so both sub-agent
//! flavors look and behave identically (same tool rows, same spinner lifecycle,
//! same plan mini-chat, same context usage footer).
//!
//! The nested loop has no call ids in its events, so ids are synthesized
//! per announcement and paired with results FIFO: the harness guarantees the
//! Nth `ToolResult`/`ToolError` matches the Nth `ToolCall` (sequential
//! ordering, see the harness event docs). A result arriving without a
//! matching announcement is dropped rather than mis-attributed.

use std::collections::VecDeque;

use serde_json::Value;

use super::events::{
    PlanEntry, PlanEntryPriority, PlanEntryStatus, SubagentEvent, ToolCallStatus, ToolDiffSummary,
    ToolKind, ToolOutputBlock,
};

/// Byte cap of one tool result's displayed tail. Mirrors the TUI's own
/// `SUBAGENT_TOOL_TAIL_BYTES`: the box keeps the TAIL of a tool output, so
/// the bridge pre-caps the event payload the same way (the TUI re-bounds,
/// the two never disagree — and a multi-megabyte bash dump never crosses
/// the event channel whole).
const RESULT_TAIL_BYTES: usize = 4_000;

/// Stateful synthesizer of typed sub-agent events from a nested harness's
/// plain event stream. One instance per internal sub-agent turn.
#[derive(Debug, Default)]
pub struct InternalEventBridge {
    /// Monotonic counter backing the synthesized call ids.
    issued: usize,
    /// Announced calls awaiting their result, FIFO: `(id, tool name)`.
    /// The harness guarantees the Nth result matches the Nth call.
    outstanding: VecDeque<(String, String)>,
}

impl InternalEventBridge {
    /// A fresh bridge for one internal sub-agent turn.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The nested loop streamed an assistant text chunk: the box's message
    /// entry (coalescing handled by the TUI, same as the ACP path).
    #[must_use]
    pub fn message(text: String) -> SubagentEvent {
        SubagentEvent::Message { text }
    }

    /// The nested loop streamed a reasoning chunk: the box's thought entry.
    #[must_use]
    pub fn thought(text: String) -> SubagentEvent {
        SubagentEvent::Thought { text }
    }

    /// A tool call was announced by the nested loop: a `ToolCall` event with
    /// a synthesized id, the tool-derived kind and title, and the raw input
    /// (the TUI extracts the same detail keys — path, query, command — it
    /// shows for ACP calls).
    pub fn tool_call(&mut self, tool: &str, input: &Value) -> SubagentEvent {
        let id = format!("internal-{}", self.issued);
        self.issued += 1;
        self.outstanding.push_back((id.clone(), tool.to_string()));
        SubagentEvent::ToolCall {
            id,
            title: tool.to_string(),
            kind: tool_kind(tool),
            status: ToolCallStatus::InProgress,
            raw_input: Some(input.clone()),
        }
    }

    /// A tool call completed with a serialized output: the FIFO-oldest
    /// outstanding call is patched to `Completed`, carrying the output tail
    /// and — for `fs_edit`-family results — a one-line diff summary parsed
    /// from the result's unified diff, exactly what an ACP `ToolCallUpdate`
    /// with a diff block renders.
    ///
    /// `None` when no call is outstanding (an unpaired result cannot be
    /// attributed to a row).
    pub fn tool_result(&mut self, output: &str) -> Option<SubagentEvent> {
        let (id, tool) = self.outstanding.pop_front()?;
        Some(SubagentEvent::ToolCallUpdate {
            id,
            status: Some(ToolCallStatus::Completed),
            title: None,
            raw_output: None,
            content: ToolOutputBlock {
                text: output_tail(output),
                skipped: 0,
                diff: diff_summary_from_result(tool, output),
            },
        })
    }

    /// A tool call failed during dispatch: the FIFO-oldest outstanding call
    /// is patched to `Failed`, carrying the error message as the row's tail
    /// (an ACP failed update renders its output text the same way).
    /// `None` when no call is outstanding.
    pub fn tool_error(&mut self, error: &str) -> Option<SubagentEvent> {
        let (id, _) = self.outstanding.pop_front()?;
        Some(SubagentEvent::ToolCallUpdate {
            id,
            status: Some(ToolCallStatus::Failed),
            title: None,
            raw_output: None,
            content: ToolOutputBlock {
                text: output_tail(error),
                skipped: 0,
                diff: None,
            },
        })
    }

    /// A `plan_todo_write` call's input mapped to the box's plan entry —
    /// the same mini-chat TODO the ACP agents emit natively. `None` when
    /// the input does not deserialize as a todo write (a malformed call is
    /// still announced as a plain tool row by [`Self::tool_call`]).
    #[must_use]
    pub fn plan_from_todo_write(input: &Value) -> Option<SubagentEvent> {
        #[derive(serde::Deserialize)]
        struct TodoItem {
            description: String,
            #[serde(default)]
            status: TodoStatus,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "snake_case")]
        enum TodoStatus {
            #[default]
            Pending,
            InProgress,
            Completed,
            Cancelled,
        }

        let todos = input.get("todos").cloned()?;
        let parsed: Vec<TodoItem> = serde_json::from_value(todos).ok()?;
        let entries: Vec<PlanEntry> = parsed
            .into_iter()
            .map(|item| PlanEntry {
                content: item.description,
                priority: match item.status {
                    TodoStatus::InProgress => PlanEntryPriority::High,
                    _ => PlanEntryPriority::Medium,
                },
                status: match item.status {
                    TodoStatus::InProgress => PlanEntryStatus::InProgress,
                    TodoStatus::Completed => PlanEntryStatus::Completed,
                    // The plan mirror has no "cancelled" state; a cancelled
                    // task renders as the neutral not-started glyph.
                    _ => PlanEntryStatus::Pending,
                },
            })
            .collect();
        (!entries.is_empty()).then_some(SubagentEvent::Plan { entries })
    }

    /// The nested harness's context snapshot mapped to the box's usage
    /// footer (`context_window` = the budget, `tokens_in_context` = the
    /// estimated tokens held) — the same numbers the ACP `UsageUpdate`
    /// carries.
    #[must_use]
    pub fn usage(total_tokens: usize, max_tokens: usize) -> SubagentEvent {
        SubagentEvent::Usage {
            context_window: u64::try_from(max_tokens).unwrap_or(u64::MAX),
            tokens_in_context: u64::try_from(total_tokens).unwrap_or(u64::MAX),
        }
    }
}

/// Display category of an internal tool name (the mirror the ACP kinds
/// render through). File reads and lookups search, mutations edit, shell
/// work executes, web work fetches.
#[must_use]
pub fn tool_kind(tool: &str) -> ToolKind {
    match tool {
        "fs_read" | "lsp_hover" => ToolKind::Read,
        "fs_write" | "fs_edit" | "fs_edit_lines" | "fs_ast_edit" | "fs_rollback" | "lsp_rename"
        | "lsp_code_actions" | "lsp_restart" => ToolKind::Edit,
        "bash_run"
        | "subagent_call"
        | "computer_apps"
        | "computer_snapshot"
        | "computer_wait"
        | "computer_screenshot"
        | "computer_act"
        | "computer_control" => ToolKind::Execute,
        "find_glob"
        | "find_grep"
        | "recall_search"
        | "skills_match_skills"
        | "lsp_definitions"
        | "lsp_references"
        | "lsp_symbols"
        | "lsp_workspace_symbols"
        | "lsp_call_hierarchy" => ToolKind::Search,
        "web_fetch" | "web_search" => ToolKind::Fetch,
        "plan_todo_write" | "skills_list" | "skills_read" | "skills_read_asset" => ToolKind::Think,
        // `ask_questions`/`stop_agent_loop` are blocklisted inside a
        // sub-agent; anything else unknown renders as the generic tool row.
        _ => ToolKind::Unknown,
    }
}

/// The displayed tail of a tool output: the LAST `RESULT_TAIL_BYTES` bytes,
/// cut at a character boundary (mirrors the TUI's bounded tail).
fn output_tail(output: &str) -> String {
    if output.len() <= RESULT_TAIL_BYTES {
        return output.to_string();
    }
    let mut start = output.len() - RESULT_TAIL_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].to_string()
}

/// One-line diff summary parsed from a serialized `fs_edit`-family result
/// (a JSON array of per-target objects whose `diff` field is a unified
/// diff). The LAST diff wins — one summary line per tool call, the same
/// rule the ACP content extraction applies to multiple diff blocks.
///
/// `None` for anything that is not an edit result with a diff (bash text,
/// grep listings, read output — those are not file modifications).
fn diff_summary_from_result(tool: String, output: &str) -> Option<ToolDiffSummary> {
    if !matches!(tool.as_str(), "fs_edit" | "fs_edit_lines" | "fs_ast_edit") {
        return None;
    }
    let results: Vec<Value> = serde_json::from_str(output).ok()?;
    results
        .iter()
        .filter_map(|entry| {
            let diff = entry.get("diff")?.as_str()?;
            let path = entry.get("path")?.as_str()?;
            let mut added = 0_u32;
            let mut removed = 0_u32;
            for line in diff.lines() {
                if line.starts_with('+') && !line.starts_with("+++") {
                    added += 1;
                } else if line.starts_with('-') && !line.starts_with("---") {
                    removed += 1;
                }
            }
            Some(ToolDiffSummary {
                path: path.to_string(),
                added,
                removed,
            })
        })
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_and_thought_map_to_the_typed_stream_variants() {
        assert!(matches!(
            InternalEventBridge::message("hi".to_string()),
            SubagentEvent::Message { text } if text == "hi"
        ));
        assert!(matches!(
            InternalEventBridge::thought("pondering".to_string()),
            SubagentEvent::Thought { text } if text == "pondering"
        ));
    }

    #[test]
    fn tool_call_announces_in_progress_with_a_synthesized_id() {
        let mut bridge = InternalEventBridge::new();
        let event = bridge.tool_call("fs_read", &serde_json::json!({"path": "src/a.rs"}));
        match event {
            SubagentEvent::ToolCall {
                id,
                title,
                kind,
                status,
                raw_input,
            } => {
                assert_eq!(id, "internal-0");
                assert_eq!(title, "fs_read");
                assert_eq!(kind, ToolKind::Read);
                assert_eq!(status, ToolCallStatus::InProgress);
                assert_eq!(raw_input, Some(serde_json::json!({"path": "src/a.rs"})));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn ids_are_unique_across_calls() {
        let mut bridge = InternalEventBridge::new();
        let first = bridge.tool_call("bash_run", &serde_json::json!({}));
        let second = bridge.tool_call("bash_run", &serde_json::json!({}));
        let id_of = |e: SubagentEvent| match e {
            SubagentEvent::ToolCall { id, .. } => id,
            other => panic!("expected ToolCall, got {other:?}"),
        };
        assert_ne!(id_of(first), id_of(second));
    }

    #[test]
    fn result_pairs_with_the_oldest_outstanding_call() {
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("bash_run", &serde_json::json!({}));
        bridge.tool_call("fs_read", &serde_json::json!({}));
        let update = bridge.tool_result("ok").expect("first result pairs");
        match update {
            SubagentEvent::ToolCallUpdate {
                id,
                status,
                content,
                ..
            } => {
                assert_eq!(id, "internal-0");
                assert_eq!(status, Some(ToolCallStatus::Completed));
                assert_eq!(content.text, "ok");
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
        let update = bridge.tool_result("data").expect("second result pairs");
        assert!(matches!(
            update,
            SubagentEvent::ToolCallUpdate { ref id, .. } if id == "internal-1"
        ));
    }

    #[test]
    fn unpaired_result_is_dropped() {
        let mut bridge = InternalEventBridge::new();
        assert!(bridge.tool_result("stray").is_none());
        assert!(bridge.tool_error("stray").is_none());
    }

    #[test]
    fn error_marks_the_call_failed() {
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("fs_write", &serde_json::json!({}));
        let update = bridge.tool_error("permission denied").expect("error pairs");
        assert!(matches!(
            update,
            SubagentEvent::ToolCallUpdate {
                status: Some(ToolCallStatus::Failed),
                ..
            }
        ));
    }

    #[test]
    fn long_output_is_tail_capped_at_a_char_boundary() {
        let long = format!("{}é", "x".repeat(RESULT_TAIL_BYTES));
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("bash_run", &serde_json::json!({}));
        let update = bridge.tool_result(&long).expect("pairs");
        match update {
            SubagentEvent::ToolCallUpdate { content, .. } => {
                assert!(content.text.len() <= RESULT_TAIL_BYTES + 2);
                assert!(content.text.ends_with('é'));
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[test]
    fn edit_result_yields_a_diff_summary_last_diff_wins() {
        let output = serde_json::json!([
            {"path": "a.rs", "diff": "+one\n+two\n-three\n"},
            {"path": "b.rs", "diff": "+only\n"}
        ])
        .to_string();
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("fs_edit", &serde_json::json!({}));
        let update = bridge.tool_result(&output).expect("pairs");
        match update {
            SubagentEvent::ToolCallUpdate { content, .. } => {
                let diff = content.diff.expect("edit carries a diff summary");
                assert_eq!(diff.path, "b.rs");
                assert_eq!(diff.added, 1);
                assert_eq!(diff.removed, 0);
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[test]
    fn non_edit_results_carry_no_diff_summary() {
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("bash_run", &serde_json::json!({}));
        let update = bridge
            .tool_result("[{\"path\": \"a.rs\", \"diff\": \"+x\"}]")
            .expect("pairs");
        match update {
            SubagentEvent::ToolCallUpdate { content, .. } => assert!(content.diff.is_none()),
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[test]
    fn malformed_edit_output_carries_no_diff_summary() {
        let mut bridge = InternalEventBridge::new();
        bridge.tool_call("fs_edit", &serde_json::json!({}));
        let update = bridge.tool_result("not json").expect("pairs");
        match update {
            SubagentEvent::ToolCallUpdate { content, .. } => assert!(content.diff.is_none()),
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[test]
    fn todo_write_input_maps_to_a_plan_entry_per_todo() {
        let input = serde_json::json!({"todos": [
            {"description": "first", "status": "in_progress"},
            {"description": "second", "status": "completed"},
            {"description": "third", "status": "cancelled"}
        ]});
        let event = InternalEventBridge::plan_from_todo_write(&input).expect("plan");
        match event {
            SubagentEvent::Plan { entries } => {
                assert_eq!(entries.len(), 3);
                assert_eq!(entries[0].content, "first");
                assert_eq!(entries[0].status, PlanEntryStatus::InProgress);
                assert_eq!(entries[0].priority, PlanEntryPriority::High);
                assert_eq!(entries[1].status, PlanEntryStatus::Completed);
                assert_eq!(entries[2].status, PlanEntryStatus::Pending);
            }
            other => panic!("expected Plan, got {other:?}"),
        }
    }

    #[test]
    fn todo_write_without_todos_yields_no_plan() {
        assert!(InternalEventBridge::plan_from_todo_write(&serde_json::json!({})).is_none());
        assert!(
            InternalEventBridge::plan_from_todo_write(&serde_json::json!({"todos": []})).is_none()
        );
        assert!(
            InternalEventBridge::plan_from_todo_write(&serde_json::json!({"todos": "no"}))
                .is_none()
        );
    }

    #[test]
    fn context_info_maps_to_the_usage_footer() {
        let event = InternalEventBridge::usage(1_234, 10_000);
        assert_eq!(
            event,
            SubagentEvent::Usage {
                context_window: 10_000,
                tokens_in_context: 1_234,
            }
        );
    }

    #[test]
    fn tool_kinds_cover_the_internal_dispatch_table() {
        assert_eq!(tool_kind("fs_read"), ToolKind::Read);
        assert_eq!(tool_kind("fs_edit_lines"), ToolKind::Edit);
        assert_eq!(tool_kind("bash_run"), ToolKind::Execute);
        assert_eq!(tool_kind("find_grep"), ToolKind::Search);
        assert_eq!(tool_kind("web_fetch"), ToolKind::Fetch);
        assert_eq!(tool_kind("plan_todo_write"), ToolKind::Think);
        assert_eq!(tool_kind("mystery"), ToolKind::Unknown);
    }
}
