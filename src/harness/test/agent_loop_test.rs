use super::super::core::Harness;
use super::super::core::INTERRUPTED_MARKER;
use super::super::core::result_is_useless;
use super::super::events::HarnessEvent;
use crate::harness::context::{ContextItem, ContextManagerState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
                        | HarnessEvent::Error { .. }
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
                        | HarnessEvent::Error { .. }
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

    // Should get tokens, tool call, tool result (test tools dispatch with a
    // canned success), then a terminal event (mock stream consumed).
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
            .any(|e| matches!(e, HarnessEvent::ToolResult { .. }))
    );
    let last = events.last().unwrap();
    assert!(
        matches!(
            last,
            HarnessEvent::Done { .. } | HarnessEvent::Stopped { .. } | HarnessEvent::Error { .. }
        ),
        "expected terminal event, got {last:?}"
    );
}

// ── Crush-style loop detection ────────────────────────────────────────────
//
// When the model repeats the EXACT same tool interaction (name + arguments +
// result) more than LOOP_DETECTION_MAX_REPEATS times within the recent
// LOOP_DETECTION_WINDOW_SIZE tool-calling iterations, the loop stops with a
// `Done` (plus a Toast) instead of burning tokens up to MAX_ITERATIONS.
// Here the model calls `test_tool` with the same arguments 20 times; the
// identical signature (canned success result) trips the detector at the
// 10th tool-calling iteration, so only 10 streams are consumed.
#[tokio::test]
async fn loop_detection_stops_repeated_identical_tool_calls() {
    // 20 identical tool-calling iterations — the detector must stop at 10.
    let streams: Vec<Result<Vec<&str>, &str>> = (0..20)
        .map(|_| Ok(vec![r#"{"name": "test_tool", "arguments": {"x": "same"}}"#]))
        .collect();
    let mut h = Harness::new_test()
        .with_test_tool(
            "test_tool",
            serde_json::json!({
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x"]
            }),
        )
        .with_mock_streams(streams);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("repeat the same call", tx, answer_rx, perm_rx, stop_signal)
            .await;
    });

    let mut events = Vec::new();
    let timeout = tokio::time::Duration::from_secs(8);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(
                    event,
                    HarnessEvent::Done { .. }
                        | HarnessEvent::Stopped { .. }
                        | HarnessEvent::Error { .. }
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

    // The 10th tool call is dispatched, THEN the detector fires — so the
    // loop stops with exactly 10 dispatched calls, not 20.
    let tool_calls = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::ToolCall { .. }))
        .count();
    assert_eq!(
        tool_calls, 10,
        "loop must stop at the 10th identical tool call, got {tool_calls}; \
         events={events:?}"
    );
    assert!(
        matches!(events.last(), Some(HarnessEvent::Done { .. })),
        "loop detection must end with Done, got {:?}",
        events.last()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Toast { .. })),
        "loop detection must surface a Toast explaining the stop"
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
            closable: false,
            ..
        }
    ));
    assert!(matches!(&items[2], ContextItem::ToolCall { .. }));
    assert!(matches!(&items[3], ContextItem::ToolResult { .. }));
    assert!(matches!(
        &items[4],
        ContextItem::Closure { content, .. } if content == "Done!"
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

// ── Proof: the LLM compaction runs in the harness ─────────────────────────
//
// When the useless-chain sweep leaves the total over the 80% trigger,
// `run_agent_loop` performs the LLM compaction: it builds the summary prompt
// from the context manager, calls the model (the mock chat response here),
// applies the summary, and re-adds the in-flight input so the model sees the
// task verbatim. The TUI gets the lifecycle events and the timeline ends as
// [Compaction, user, LoopClosure].
#[tokio::test]
async fn run_agent_loop_runs_the_llm_compaction_when_over_the_trigger() {
    use crate::harness::context::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    // Small budget so the preloaded history overflows the 80% trigger with
    // nothing for the useless-chain sweep to remove, forcing the LLM
    // compaction.
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a LoopClosure
    // below): history over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
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
                        | HarnessEvent::Error { .. }
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
    use crate::harness::context::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a LoopClosure
    // below): history over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
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
                        | HarnessEvent::Error { .. }
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

// ── Abandoned input cleanup: Esc before any output, then a new input ─────
//
// The user sends input A and cancels (Esc) before the LLM produced anything,
// then sends input B. The harness rebuilds with_history(A) and B lands
// directly on top of A — two consecutive user turns with no output between.
// The abandoned A must be dropped from the model context; B is sent exactly
// once.
#[tokio::test]
async fn abandoned_input_is_dropped_when_a_new_input_follows() {
    let mut h = Harness::new_test()
        .with_history(&[("user".into(), "abandoned input A".into())])
        .with_mock_stream(Ok(vec!["final answer"]));

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    h.run_agent_loop(
        "fresh input B",
        tx,
        answer_rx,
        perm_rx,
        Arc::new(AtomicBool::new(false)),
    )
    .await;

    let msgs = h.build_messages_for_test("");
    let texts: Vec<&str> = msgs.iter().filter_map(|m| m.content.as_deref()).collect();
    assert!(
        !texts.contains(&"abandoned input A"),
        "the abandoned input must not reach the model; texts={texts:?}"
    );
    assert_eq!(
        texts.iter().filter(|t| **t == "fresh input B").count(),
        1,
        "the fresh input is sent exactly once; texts={texts:?}"
    );
}

// A run that DID produce output before the cancel keeps its input: the next
// input is a normal follow-up turn, not a replacement.
#[tokio::test]
async fn input_with_output_before_cancel_is_kept() {
    let mut h = Harness::new_test()
        .with_history(&[
            ("user".into(), "answered input A".into()),
            ("assistant".into(), "an answer already streamed".into()),
        ])
        .with_mock_stream(Ok(vec!["final answer"]));

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    h.run_agent_loop(
        "follow-up B",
        tx,
        answer_rx,
        perm_rx,
        Arc::new(AtomicBool::new(false)),
    )
    .await;

    let msgs = h.build_messages_for_test("");
    let texts: Vec<&str> = msgs.iter().filter_map(|m| m.content.as_deref()).collect();
    assert!(
        texts.contains(&"answered input A"),
        "a turn with output after it is kept; texts={texts:?}"
    );
    assert!(
        texts.contains(&"an answer already streamed"),
        "the previous output stays in context; texts={texts:?}"
    );
}

// ── Context-window overflow recovery (provider rejects the prompt size) ──

// When the summarizer call fails with a context-window overflow, the harness
// drives the hierarchical MapReduce contingency. When the checkpoint itself cannot
// fit the context into the window (the summarizer keeps overflowing), the
// provider is marked stuck and the user is notified through a Toast — the
// loop still completes normally. The checkpoint never drains items: the tool chains
// stay in the timeline.
#[tokio::test]
async fn run_agent_loop_marks_provider_stuck_when_map_reduce_cannot_fit() {
    use crate::harness::context::ContextManager;
    use crate::harness::core::CONTEXT_WINDOW_MARKER;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a Closure
    // below): history over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // Two tool chains in the timeline (the checkpoint summarizes, never drains).
    h.context_manager.add_tool_call("t0", "fs_read", "{}");
    h.context_manager.add_tool_result("t0", "contents A");
    h.context_manager.add_tool_call("t1", "fs_read", "{}");
    h.context_manager.add_tool_result("t1", "contents B");
    h = h
        .with_mock_chat(Err(CONTEXT_WINDOW_MARKER)) // summarizer: always overflow
        .with_mock_stream(Ok(vec!["final answer"]));

    // The harness (with its context manager) is moved into the task; the CM
    // state after the loop is reported back through this channel.
    let (state_tx, mut state_rx) =
        tokio::sync::mpsc::unbounded_channel::<(bool, Vec<ContextItem>)>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let _ = state_tx.send((
            h.context_manager.overflow_stuck("gpt-4o-mini"),
            h.context_manager.items_snapshot(),
        ));
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
                        | HarnessEvent::Error { .. }
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
    let (stuck, items) = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or((false, Vec::new()));
    handle.abort();

    // The exhausted overflow surfaces a persistent warning to the user.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::Toast {
                variant: ToastVariant::Warning,
                ..
            }
        )),
        "the overflow must surface a warning toast; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "the compaction is reported as failed; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes after the overflow is handled"
    );
    // The checkpoint never drains items — the tool chains stay in the timeline.
    assert!(
        items.iter().any(|it| {
            matches!(
                it,
                ContextItem::ToolCall { .. } | ContextItem::ToolResult { .. }
            )
        }),
        "the checkpoint summarizes, never drains — the tool chains remain"
    );
    // The provider is recorded as stuck (the notification persists).
    assert!(
        stuck,
        "the provider must be marked stuck when the checkpoint cannot fit the context"
    );
}

// A generic (non-context-window) summarizer error is retried
// MAX_COMPACTION_RETRIES times with backoff, then surfaces an Error toast;
// the loop still completes.
#[tokio::test]
async fn run_agent_loop_retries_generic_compaction_failures_then_notifies() {
    use crate::harness::context::ContextManager;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a LoopClosure
    // below): history over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    h = h
        .with_mock_chat(Err("provider exploded")) // generic failure, every attempt
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
    let timeout = tokio::time::Duration::from_secs(8);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_done = matches!(
                    event,
                    HarnessEvent::Done { .. }
                        | HarnessEvent::Stopped { .. }
                        | HarnessEvent::Error { .. }
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
            HarnessEvent::Toast {
                variant: ToastVariant::Error,
                ..
            }
        )),
        "the generic failure must surface an error toast; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "the compaction is reported as failed; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes after the retries are exhausted"
    );
}

// Once the provider is recorded as stuck (the checkpoint can no longer shrink the
// context into the window), the doomed summarizer call is SKIPPED — no
// LlmCompaction lifecycle events — and the throttled warning toast is
// re-surfaced instead.
#[tokio::test]
async fn run_agent_loop_skips_the_doomed_summarizer_when_stuck() {
    use crate::harness::context::ContextManager;
    use crate::harness::events::ToastVariant;

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a LoopClosure
    // below): history over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // The provider is already recorded as overflowing (marked stuck).
    h.context_manager.mark_overflow("gpt-4o-mini");
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
                        | HarnessEvent::Error { .. }
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

    // The user is still reminded (throttled toast)…
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::Toast {
                variant: ToastVariant::Warning,
                ..
            }
        )),
        "the stuck overflow must keep surfacing the warning; events={events:?}"
    );
    // …but the doomed summarizer call is NOT made: no lifecycle events.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, HarnessEvent::LlmCompaction { .. })),
        "no summarizer call when the provider is stuck; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes without the summarizer"
    );
}

// ── Hierarchical MapReduce (the known-window contingency) ────────────────
//
// When the ACTIVE model's window is KNOWN (discovery or a context-window
// error that reported it) and the held context exceeds it — the
// model-switch-to-a-smaller-window scenario — the harness drives the checkpoint
// instead of the single-shot compaction: the timeline is summarized in
// independent maps, globally reduced, validated, and committed ATOMICALLY as
// a checkpoint. This tiny-window fixture folds its oversized raw prefix whole.
#[tokio::test]
async fn known_window_overflow_drives_map_reduce_and_commits_the_anchor() {
    use crate::harness::context::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    // The ACTIVE model's window is KNOWN (discovery succeeded) and far below
    // the held context: the single-shot compaction transcript would overflow
    // the provider, so the checkpoint path must fire instead.
    h = h.with_discovered_window(600);
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a LoopClosure
    // below): history over the trigger (~2200 ≥ 1600) and > 600 — the
    // known window.
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // The mock CHAT response is the SPLIT summarizer — every chunk call gets
    // the same summary (the driver advances the cursor with each one); the
    // mock STREAM is the loop's final answer.
    h = h
        .with_mock_chats(vec![
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("map"),
            Ok("## Objective\n- summarized chunk"),
            Ok("PASS"),
        ])
        .with_mock_stream(Ok(vec!["final answer"]));

    // The harness (with its context manager) is moved into the task; the CM
    // state after the loop is reported back through this channel.
    let (state_tx, mut state_rx) =
        tokio::sync::mpsc::unbounded_channel::<(ContextManagerState, usize)>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let info = h.context_manager.display_info();
        let _ = state_tx.send((h.context_manager.save_state(), info.total_tokens));
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
                        | HarnessEvent::Error { .. }
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
    let (state, total_tokens) =
        tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
            .await
            .ok()
            .flatten()
            .unwrap_or((ContextManagerState::default(), 0));
    let mut restored_manager = ContextManager::new(state.max_tokens);
    restored_manager.restore_state(&state);
    let model_messages = restored_manager.build_messages("");
    let items = state.items;
    handle.abort();

    // The checkpoint surfaced the SAME continuous "Summarizing" box to the TUI.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started
            }
        )),
        "the checkpoint must emit LlmCompaction::Started; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the checkpoint must emit LlmCompaction::Finished; events={events:?}"
    );
    // The chunk summaries streamed to the TUI as one continuous box.
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::LlmCompactionToken { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        streamed.contains("summarized chunk"),
        "the chunk summaries must stream as LlmCompactionToken; streamed={streamed:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes after the checkpoint; events={events:?}"
    );

    // The checkpoint committed append-only: the validated result became ONE Compaction
    // anchor appended after the (now hidden) pre-checkpoint timeline.
    let anchor_idx = items
        .iter()
        .position(|it| {
            matches!(it, ContextItem::Compaction { summary, .. }
            if summary.contains("summarized chunk"))
        })
        .expect("the buffer is committed as the new anchor");
    assert!(
        items.iter().take(anchor_idx).any(|it| matches!(
            it,
            ContextItem::User { original, .. } | ContextItem::Assistant { original, .. }
                if original.starts_with("u ") || original.starts_with("a ")
        )),
        "the giant history is RETAINED behind the anchor (append-only)"
    );
    // The in-flight input remains as the recent raw tail. Because the
    // checkpoint is append-only it is physically written after that input,
    // while model composition places the checkpoint before the raw tail.
    assert!(
        items
            .iter()
            .any(|item| matches!(item, ContextItem::User { original, .. } if original == "hi")),
        "the in-flight input remains verbatim after MapReduce; items={items:?}"
    );
    assert!(
        matches!(
            &items[anchor_idx + 1],
            ContextItem::Closure { content, .. } if content == "final answer"
        ),
        "the loop's final answer follows the anchor; items={items:?}"
    );
    // The model sees only: anchor, in-flight input, final answer.
    assert_eq!(
        model_messages.len(),
        3,
        "anchor + in-flight input + final answer"
    );
    assert!(
        !model_messages.iter().any(|message| message
            .content
            .as_deref()
            .is_some_and(|content| content.starts_with("u "))),
        "the pre-checkpoint history is hidden behind the boundary"
    );
    // Early containment: the committed anchor fits the known window.
    assert!(
        total_tokens <= 600,
        "the anchor must fit the known window; total_tokens={total_tokens}"
    );
}

// The REACTIVE fork: when the single-shot compaction transcript itself
// overflows with a REPORTED window, `llm_compact` drives the checkpoint — the
// checkpoint shrinks the whole timeline (every item included) into a fitting
// anchor and the compaction resolves successfully
// (Finished, never Failed; the provider is never marked stuck).
#[tokio::test]
async fn reactive_overflow_reports_window_and_drives_map_reduce_inside_llm_compact() {
    use crate::harness::context::ContextManager;
    use crate::harness::core::CONTEXT_WINDOW_MARKER;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    // NO discovered window: the loop-start PROACTIVE fork must NOT fire, so
    // `llm_compact` runs the single-shot compaction — whose transcript then
    // overflows WITH a reported window, triggering the reactive checkpoint.
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // History over the trigger (~2200 ≥ 1600) and > 600 (the
    // window the error will report).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // Mock CHAT queue: call 1 = the single-shot compaction summarizer, which
    // reports a 600-token ContextWindow. Eight independent maps cover the two
    // oversized source items, one reducer reconciles all maps, and a final
    // validation passes. The in-flight "hi" stays in the recent raw tail.
    let overflow = format!("{CONTEXT_WINDOW_MARKER}:600");
    h = h
        .with_mock_chats(vec![
            Err(overflow.as_str()),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Source Ranges\n- reactive map"),
            Ok("## Objective\n- reactive final"),
            Ok("PASS"),
        ])
        .with_mock_stream(Ok(vec!["final answer"]));

    let (state_tx, mut state_rx) =
        tokio::sync::mpsc::unbounded_channel::<(ContextManagerState, bool)>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let _ = state_tx.send((
            h.context_manager.save_state(),
            h.context_manager.overflow_stuck("gpt-4o-mini"),
        ));
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
                        | HarnessEvent::Error { .. }
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
    let (state, stuck) = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or((ContextManagerState::default(), false));
    let mut restored_manager = ContextManager::new(state.max_tokens);
    restored_manager.restore_state(&state);
    let model_messages = restored_manager.build_messages("");
    let items = state.items;
    handle.abort();

    // The single-shot overflow escalated into the checkpoint, which SUCCEEDED.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the reactive checkpoint must finish successfully; events={events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "the compaction must NOT be reported as failed; events={events:?}"
    );
    // The checkpoint resolved without a persistent overflow warning toast.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            HarnessEvent::Toast {
                variant: ToastVariant::Warning,
                ..
            }
        )),
        "the checkpoint resolves the overflow without the stuck-provider warning; events={events:?}"
    );
    // Intermediate maps never leak into the final compaction display.
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::LlmCompactionToken { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !streamed.contains("reactive map") && streamed.contains("reactive final"),
        "independent maps must not interleave into the checkpoint body; streamed={streamed:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes; events={events:?}"
    );

    // Append-only commit: the validated checkpoint is appended; the old
    // timeline is retained and covered only in the derived view.
    let anchor = items.iter().find_map(|it| match it {
        ContextItem::Compaction { summary, .. } if summary.contains("reactive final") => {
            Some(summary.clone())
        }
        _ => None,
    });
    assert!(
        anchor.is_some(),
        "the reduced checkpoint is the new anchor; items={items:?}"
    );
    assert!(
        items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } | ContextItem::Assistant { original, .. }
                if original.starts_with("u ") || original.starts_with("a ")
        )),
        "the giant history is RETAINED behind the anchor (append-only)"
    );
    // The model sees only the anchor and whatever came after it.
    assert!(
        !model_messages.iter().any(|message| message
            .content
            .as_deref()
            .is_some_and(|content| content.starts_with("u "))),
        "the pre-checkpoint history never reaches the model"
    );
    // The reactive checkpoint resolves without marking the provider stuck.
    assert!(
        !stuck,
        "the reactive checkpoint must not mark the provider stuck"
    );
}

// All-or-nothing: an EMPTY chunk summary (the model returned nothing for a
// chunk) is treated as a checkpoint failure — the checkpoint aborts and the timeline
// stays exactly as it was. Nothing is silently dropped from the anchor.
#[tokio::test]
async fn map_reduce_aborts_on_an_empty_chunk_summary_and_keeps_the_context() {
    use crate::harness::context::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    h = h.with_discovered_window(600);
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // History over the trigger AND over the known window.
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // The SPLIT summarizer returns EMPTY for every chunk; the mock STREAM is
    // the loop's final answer.
    h = h
        .with_mock_chat(Ok(""))
        .with_mock_stream(Ok(vec!["final answer"]));

    let (state_tx, mut state_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<ContextItem>>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let _ = state_tx.send(h.context_manager.items_snapshot());
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
                        | HarnessEvent::Error { .. }
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
    let items = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    handle.abort();

    // The checkpoint was attempted and ABORTED — surfaced as a failed compaction.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "the empty-summary checkpoint must report LlmCompaction::Failed; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes after the abort; events={events:?}"
    );
    // Atomicity: the giant history was NOT folded — nothing was committed.
    assert!(
        items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } if original.starts_with("u ")
        )),
        "the timeline keeps the original history; items={items:?}"
    );
    assert!(
        !items
            .iter()
            .any(|it| matches!(it, ContextItem::Compaction { .. })),
        "no anchor was committed — all or nothing; items={items:?}"
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

// ── Incremental persistence: ContextSnapshot events ───────────────────────
//
// The harness emits a context snapshot per tool dispatch (with a test-zeroed
// cadence) so the TUI can persist the session context records mid-run. This test
// proves the snapshots (1) arrive during the run, (2) are valid
// [`ContextManagerState`]s,
// (3) reflect progress — each later snapshot owns the tool call/result
// accumulated since — and (4) restore into a fresh manager that keeps
// answering: exactly the round trip a crash/restart resume performs.
#[tokio::test]
async fn run_agent_loop_emits_resumable_incremental_context_snapshots() {
    use crate::harness::context::{ContextManager, ContextManagerState};

    let mut h = Harness::new_test()
        .with_snapshot_interval(std::time::Duration::ZERO)
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
                "call 1 ",
                r#"{"name": "test_tool", "arguments": {"x": "one"}}"#,
            ]),
            Ok(vec![
                "call 2 ",
                r#"{"name": "test_tool", "arguments": {"x": "two"}}"#,
            ]),
            Ok(vec!["final answer"]),
        ]);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("incremental", tx, answer_rx, perm_rx, stop_signal)
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
                        | HarnessEvent::Error { .. }
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

    let states: Vec<ContextManagerState> = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::ContextSnapshot { context } => Some(context.clone()),
            _ => None,
        })
        .collect();
    assert!(
        states.len() >= 2,
        "a multi-dispatch run must snapshot per dispatch; got {}",
        states.len()
    );

    // Progress: the snapshots accumulate the tool call/result pairs.
    let sizes: Vec<usize> = states.iter().map(|s| s.items.len()).collect();
    assert!(
        sizes.windows(2).all(|w| w[0] < w[1]),
        "each later snapshot must own strictly more accumulated context; sizes={sizes:?}"
    );

    // Resumability: restore the LAST snapshot into a fresh manager and keep
    // answering — the exact session-log round trip on restart.
    let last = states.last().expect("non-empty snapshots");
    let mut restored = ContextManager::new(last.max_tokens);
    restored.restore_state(last);
    let joined: String = restored
        .build_messages("")
        .into_iter()
        .filter_map(|m| m.content)
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        joined.contains("call 1 ") && joined.contains("call 2 "),
        "the restored context must own the accumulated tool turns; joined={joined:?}"
    );
}

// ── Proof: user messages queued for the NEXT REQUEST are injected ────────
//
// The "next request" queue enters the loop via `run_agent_loop_with_queued_input`:
// messages waiting on the channel before a request are recorded as regular
// protected user turns and acknowledged with `UserMessageInjected` (FIFO),
// so the TUI can move them out of its pending area into normal history.
#[tokio::test]
async fn queued_next_request_messages_are_injected_into_context() {
    let mut h = Harness::new_test().with_mock_stream(Ok(vec!["ok"]));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    // Pre-queue two messages (FIFO) before the loop starts.
    let (queued_tx, queued_rx) = tokio::sync::mpsc::unbounded_channel();
    queued_tx.send("fix the test".to_string()).unwrap();
    queued_tx.send("and the docs".to_string()).unwrap();

    // Run directly (like bug_hunt::run_loop_and_collect) so `h` is still
    // owned by this test after the loop terminates; the mock stream makes the
    // run deterministic and fast.
    h.run_agent_loop_with_queued_input("hello", tx, answer_rx, perm_rx, stop_signal, queued_rx)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // Both queued messages acknowledged in FIFO order.
    let injected: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::UserMessageInjected { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(injected, vec!["fix the test", "and the docs"]);

    // And they landed in the context manager as protected user turns, in
    // order, right after the loop input.
    let items: Vec<ContextItem> = h.context_manager.items_snapshot();
    let users: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            ContextItem::User { original, .. } => Some(original.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["hello", "fix the test", "and the docs"]);
}

// ── Proof: an injected message is part of the NEXT request ───────────────
//
// A message queued before the loop starts is drained at iteration 1 and so
// appears in the SAME request that carries the tool call (the model sees it
// before it acts), and the loop still continues through its tool chain to a
// normal finish.
#[tokio::test]
async fn queued_next_request_message_enters_the_next_request_with_tool_work() {
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
            Ok(vec![r#"{"name": "test_tool", "arguments": {"x": "1"}}"#]),
            Ok(vec!["all done"]),
        ]);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let (queued_tx, queued_rx) = tokio::sync::mpsc::unbounded_channel();
    queued_tx.send("follow up".to_string()).unwrap();

    h.run_agent_loop_with_queued_input("use tool", tx, answer_rx, perm_rx, stop_signal, queued_rx)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // The queued message was injected (acknowledged) and the loop finished.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::UserMessageInjected { text } if text == "follow up"
        )),
        "expected a UserMessageInjected event; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. }))
    );

    // The context sequence places the injected user turn in the SAME request
    // as the tool call — before it — and the loop still completes normally.
    let items: Vec<ContextItem> = h.context_manager.items_snapshot();
    let user_positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| {
            matches!(it, ContextItem::User { original, .. } if original == "follow up").then_some(i)
        })
        .collect();
    assert_eq!(
        user_positions.len(),
        1,
        "the injected message must appear exactly once; items={items:?}"
    );
    let toolcall_pos = items
        .iter()
        .position(|it| matches!(it, ContextItem::ToolCall { .. }));
    assert!(
        toolcall_pos.is_some_and(|t| user_positions[0] < t),
        "the injected message must be delivered before the tool call it precedes; items={items:?}"
    );
    assert!(matches!(
        items.last(),
        Some(ContextItem::Closure { content, .. }) if content == "all done"
    ));
}

/// ESC pressed MID-STREAM must stop the agent loop — never complete the
/// truncated turn. Regression: the streaming chunk loop used to `break` on
/// the stop signal and fall through to `Ok("done")`, so `run_agent_loop`
/// treated the interrupted response as a finished assistant turn and emitted
/// a spurious `Done` (and with queued messages, the TUI auto-started the
/// NEXT loop — the run appeared to keep going after ESC).
#[tokio::test]
async fn test_agent_loop_stop_mid_stream() {
    let mut h = Harness::new_test()
        .with_mock_stream(Ok(vec!["Hello", " world", " more", " done"]))
        // 100ms between tokens gives the test a wide window to flip the
        // stop signal while the stream is still running.
        .with_mock_stream_delay_ms(100);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let stop_for_loop = stop_signal.clone();
    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_for_loop)
            .await;
    });

    // Let the first token land, then press ESC "mid-stream".
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    stop_signal.store(true, Ordering::Relaxed);

    // Collect events until the terminal one (bounded so a regression that
    // keeps the loop running fails instead of hanging the suite).
    let mut terminal: Option<HarnessEvent> = None;
    let timeout = tokio::time::Duration::from_secs(5);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                let is_terminal = matches!(
                    event,
                    HarnessEvent::Done { .. }
                        | HarnessEvent::Stopped { .. }
                        | HarnessEvent::Error { .. }
                );
                if is_terminal {
                    terminal = Some(event);
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    let _ = handle.abort();

    // The loop MUST report `Stopped` — not `Done` (the turn was interrupted,
    // not completed) and not `Error`.
    assert!(
        matches!(terminal, Some(HarnessEvent::Stopped { .. })),
        "mid-stream ESC must stop the loop; got terminal={terminal:?}"
    );
}

/// Direct contract: a stream interrupted by the stop signal must return the
/// `INTERRUPTED_MARKER` error, never `Ok`. (Pre-set signal — the mid-stream
/// path is covered by `test_agent_loop_stop_mid_stream` above.)
#[tokio::test]
async fn test_stream_chat_with_messages_stop_yields_interrupted_marker() {
    let mut h = Harness::new_test().with_mock_stream(Ok(vec!["Hello", " world"]));
    let stop_signal = Arc::new(AtomicBool::new(true));
    h.set_stop_signal_for_test(stop_signal);

    let result = h.stream_chat_with_messages("sys", &[], |_| {}).await;
    assert_eq!(result.unwrap_err(), INTERRUPTED_MARKER);
}
