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
async fn test_agent_loop_forwards_compaction_phases_to_the_tui() {
    use crate::harness::context_manager::{CompactionEvent, ContextManager};

    // ~5.8 chars/token with the default encoding, so 200 paragraphs of ~63
    // chars ≈ 2100 tokens — comfortably over the 1600-token (80%) trigger.
    let big_answer = (0..200)
        .map(|i| format!("Answer paragraph {i} discusses rivers, mountains and weather. "))
        .collect::<String>();

    let mut h = Harness::new_test();
    // Small budget so the preloaded history overflows the 80% trigger on the
    // very first `run()` inside the loop — the pipeline then compresses the
    // assistant history and the observer forwards the phases.
    h.context_manager = ContextManager::new(2000);
    h = h.with_history(&[
        ("user".to_string(), "initial question".to_string()),
        ("assistant".to_string(), big_answer),
    ]);
    h = h.with_mock_stream(Ok(vec!["final answer"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
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

    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::Compaction {
                event: CompactionEvent::PipelineStarted
            }
        )),
        "the harness must forward PipelineStarted to the TUI; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::Compaction {
                event: CompactionEvent::PipelineFinished
            }
        )),
        "the harness must forward PipelineFinished to the TUI; events={events:?}"
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
    assert!(matches!(&items[0], ContextItem::User { .. }));
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

// ── Proof: the LLM compaction (phase 3) runs in the harness ──────────────
//
// When the deterministic phases exhaust every draft and the total is still
// over the 80% trigger, `run_agent_loop` performs the LLM compaction: it
// builds the summary prompt from the context manager, calls the model (the
// mock chat response here), applies the summary, and re-adds the in-flight
// input so the model sees the task verbatim. The TUI gets the lifecycle
// events and the timeline ends as [Compaction, user, LoopClosure].
#[tokio::test]
async fn run_agent_loop_runs_the_llm_compaction_when_drafts_are_exhausted() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    // Small budget so the preloaded protected-only history overflows the 80%
    // trigger with NOTHING for the deterministic phases to compress or evict
    // (user prompts are protected by construction).
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("user".into(), "v ".repeat(1100)),
    ]);
    // The mock CHAT response is the compaction summary (the harness asks the
    // model for it); the mock STREAM is the loop's final answer.
    h = h
        .with_mock_chat(Ok("## Objective\n- Compact the session"))
        .with_mock_stream(Ok(vec!["final answer"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
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

    // The harness surfaced the LLM compaction lifecycle to the TUI.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started
            }
        )),
        "the harness must emit LlmCompaction::Started; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the harness must emit LlmCompaction::Finished; events={events:?}"
    );
    // The summarizer STREAMS its tokens to the TUI (the "Summarizing" box):
    // the mock chat response must arrive as LlmCompactionToken events.
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::LlmCompactionToken { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        streamed.contains("## Objective"),
        "the summary text must stream as LlmCompactionToken; streamed={streamed:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completed normally after the compaction"
    );
}

// When the summarization call FAILS (connector error, empty output), the
// harness must surface `LlmCompaction::Failed` and continue the loop with the
// context as it was (over budget but alive) — never panic or drop the turn.
#[tokio::test]
async fn run_agent_loop_survives_a_failed_llm_compaction() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("user".into(), "v ".repeat(1100)),
    ]);
    // The mock CHAT response is the FAILING summarization call; the mock
    // STREAM is the loop's final answer. The loop must still complete.
    h = h
        .with_mock_chat(Err("provider exploded"))
        .with_mock_stream(Ok(vec!["final answer"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
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

    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started
            }
        )),
        "the harness must attempt the LLM compaction; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "a failing summarization call must surface LlmCompaction::Failed; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop must continue and complete even when the compaction fails"
    );
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

    // find_glob speaks the same contract: a zero-match GlobOutput is useless,
    // a timed-out partial is NOT (it is an incomplete scan, not a dead end).
    assert!(result_is_useless(
        "find_glob",
        r#"{"matches":[],"total":0,"useless":true,"note":"No files found matching pattern"}"#
    ));
    assert!(!result_is_useless(
        "find_glob",
        r#"{"matches":[],"total":0,"timed_out":true,"note":"incomplete"}"#
    ));
    assert!(!result_is_useless("find_glob", "not json"));

    // The gate is per-tool: no other tool's JSON is ever interpreted.
    assert!(!result_is_useless("fs_read", r#"{"useless":true}"#));
}
