use super::super::core::Harness;
use super::super::core::result_is_useless;
use super::super::events::HarnessEvent;
use crate::harness::context_manager::ContextItem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn test_agent_loop_simple_conversation() {
    let mut h = Harness::new_test().with_mock_stream(Ok(vec!["Hello", " world"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    // Run in a task so we can collect events
    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
    });

    // Collect events with timeout
    let mut events = Vec::new();
    let timeout = tokio::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(
                    event,
                    HarnessEvent::Done { .. }
                        | HarnessEvent::Stopped { .. }
                        | HarnessEvent::Error(_)
                );
                events.push(event);
                if is_done {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    handle.abort();

    // Should get tokens then Done
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Token { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. }))
    );
}

#[tokio::test]
async fn test_agent_loop_with_tool_call() {
    let mut h = Harness::new_test()
        .with_test_tool(
            "test_tool",
            serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}),
        )
        .with_mock_stream(Ok(vec![
            "Let me call a tool ",
            r#"{"name": "test_tool", "arguments": {"x": "test"}}"#,
            " done",
        ]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("use tool", tx, answer_rx, perm_rx, stop_signal)
            .await;
    });

    let mut events = Vec::new();
    let timeout = tokio::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(
                    event,
                    HarnessEvent::Done { .. }
                        | HarnessEvent::Stopped { .. }
                        | HarnessEvent::Error(_)
                );
                events.push(event);
                if is_done {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    handle.abort();

    // Should get tokens, tool call, tool error (no server), then Error (mock consumed)
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Token { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::ToolCall { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::ToolError { .. }))
    );
    let last = events.last().unwrap();
    assert!(
        matches!(
            last,
            HarnessEvent::Done { .. } | HarnessEvent::Stopped { .. } | HarnessEvent::Error(_)
        ),
        "expected terminal event, got {last:?}"
    );
}

// ── Proof: a full multi-iteration tool loop owns the whole conversation ──
//
// Runs run_agent_loop end-to-end (iteration 1: text + tool call; iteration 2:
// final answer) and asserts the ContextManager owns the exact item sequence
// [user, assistant(text, non-compressible), tool_call, tool_result,
// loop_closure] and that build_messages renders the native roles in order
// with the user input appearing exactly once — proving no reordering, no
// content loss, and no double-send across the real agent-loop flow.
#[tokio::test]
async fn full_tool_loop_builds_correct_item_sequence_and_messages() {
    let mut h = Harness::new_test()
        .with_test_tool(
            "test_tool",
            serde_json::json!({
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x"]
            }),
        )
        .with_mock_streams(vec![
            Ok(vec![
                "Let me call a tool ",
                r#"{"name": "test_tool", "arguments": {"x": "test"}}"#,
            ]),
            Ok(vec!["Done!"]),
        ]);

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    // Run directly (like bug_hunt::run_loop_and_collect) so `h` is still
    // owned by this test after the loop terminates.
    h.run_agent_loop("use tool", tx, answer_rx, perm_rx, stop_signal)
        .await;

    // The CM owns the whole conversation with the exact expected sequence.
    let items: Vec<ContextItem> = h.context_manager.items_snapshot();
    assert_eq!(
        items.len(),
        5,
        "unexpected item sequence: {:?}",
        items
            .iter()
            .map(|i| format!("{:?}", std::mem::discriminant(i)))
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        &items[0],
        ContextItem::User {
            protected: true,
            ..
        }
    ));
    assert!(matches!(
        &items[1],
        ContextItem::Assistant {
            compressible: false,
            ..
        }
    ));
    assert!(matches!(&items[2], ContextItem::ToolCall { .. }));
    assert!(matches!(&items[3], ContextItem::ToolResult { .. }));
    assert!(matches!(
        &items[4],
        ContextItem::LoopClosure { content, .. } if content == "Done!"
    ));

    // The rendered payload: native roles in order, input exactly once.
    let msgs = h.build_messages_for_test("");
    let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "assistant", "tool", "assistant"]
    );
    assert_eq!(msgs[0].content.as_deref(), Some("use tool"));
    assert_eq!(msgs[1].content.as_deref(), Some("Let me call a tool "));
    let tc = msgs[2].tool_calls.as_ref().expect("tool_call message");
    assert_eq!(tc[0].function.name, "test_tool");
    assert_eq!(
        msgs[3].tool_call_id.as_deref(),
        Some(tc[0].id.as_str()),
        "tool result must reference the matching call id"
    );
    assert_eq!(msgs[4].content.as_deref(), Some("Done!"));
    let user_count = msgs
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref() == Some("use tool"))
        .count();
    assert_eq!(user_count, 1, "the user input must be sent exactly once");
}

// The useless bridge: `result_is_useless` parses the find_grep result JSON
// (which the tool serializes as its whole GrepOutput) and reads the `useless`
// flag. This is the exact contract that turns a zero-match search into a
// useless-tagged chain in the context manager.
#[test]
fn result_is_useless_reads_find_grep_json_contract() {
    // Realistic zero-match GrepOutput: `useless: true` + note, no matches.
    assert!(result_is_useless(
        "find_grep",
        r#"{"matches":[],"files":[],"total_matches":0,"useless":true,"note":"No matches found"}"#
    ));

    // A useful result — matches present, no flag (the tool omits it).
    assert!(!result_is_useless(
        "find_grep",
        r#"{"matches":[{"path":"a.rs","line_number":1}],"total_matches":1}"#
    ));
    // Explicit false stays false.
    assert!(!result_is_useless(
        "find_grep",
        r#"{"matches":[],"useless":false}"#
    ));
    // Malformed JSON is never interpreted as useless.
    assert!(!result_is_useless("find_grep", "not json"));

    // The gate is per-tool: no other tool's JSON is ever interpreted.
    assert!(!result_is_useless(
        "fs_read",
        r#"{"useless":true}"#
    ));
}
