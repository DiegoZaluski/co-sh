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
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
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
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
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
// drains ONE tool chain per attempt ("1 por vez") and retries; once every
// chain is gone it marks the provider stuck and notifies the user through a
// Toast — the loop still completes normally on the next main request.
#[tokio::test]
async fn run_agent_loop_drains_tool_chains_on_context_window_overflow() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::core::CONTEXT_WINDOW_MARKER;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // Two tool chains the harness can drain (the mock CHAT response is the
    // summarizer call).
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
            h.context_manager.overflow_stuck("openai"),
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
    // Every tool chain was drained from the timeline (call + result).
    assert!(
        !items.iter().any(|it| {
            matches!(
                it,
                ContextItem::ToolCall { .. } | ContextItem::ToolResult { .. }
            )
        }),
        "all tool chains were drained"
    );
    // The provider is recorded as stuck (the notification persists).
    assert!(
        stuck,
        "the provider must be marked stuck after the chains are exhausted"
    );
}

// A generic (non-context-window) summarizer error is retried
// MAX_COMPACTION_RETRIES times with backoff, then surfaces an Error toast;
// the loop still completes.
#[tokio::test]
async fn run_agent_loop_retries_generic_compaction_failures_then_notifies() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
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

// Once the provider is recorded as stuck (every chain drained, still over),
// the doomed summarizer call is SKIPPED — no LlmCompaction lifecycle events —
// and the throttled warning toast is re-surfaced instead.
#[tokio::test]
async fn run_agent_loop_skips_the_doomed_summarizer_when_stuck() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::ToastVariant;

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // The provider is already known to overflow with every chain drained.
    h.context_manager.mark_overflow("openai");
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

// The MAIN request path also recovers: when the provider rejects the agent
// request (not just the summarizer), the harness drains one chain and retries
// with REBUILT messages (the removed call/result pair must not be re-sent).
#[tokio::test]
async fn run_agent_loop_drains_chains_on_main_request_overflow() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::core::CONTEXT_WINDOW_MARKER;

    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger, and the loop input lands after
    // an output — never directly on a user turn (that pattern is the
    // abandoned-input case, which the harness now drops).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // The provider is already stuck (the summarizer is skipped) — the
    // overflow now hits the MAIN request instead.
    h.context_manager.mark_overflow("openai");
    h.context_manager.add_tool_call("t0", "fs_read", "{}");
    h.context_manager.add_tool_result("t0", "contents");
    h = h.with_mock_streams(vec![
        Err(CONTEXT_WINDOW_MARKER), // main request: overflow → drain one chain
        Ok(vec!["final answer"]),   // retry with rebuilt messages: succeeds
    ]);

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
    let items = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    handle.abort();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the retry succeeds and the loop completes; events={events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, HarnessEvent::Error(_))),
        "no terminal error after the drain retry"
    );
    assert!(
        !items.iter().any(|it| {
            matches!(
                it,
                ContextItem::ToolCall { .. } | ContextItem::ToolResult { .. }
            )
        }),
        "the drained chain is not re-sent"
    );
}

// ── Split-and-concatenate (the known-window contingency) ────────────────
//
// When the ACTIVE model's window is KNOWN (discovery or a context-window
// error that reported it) and the held context exceeds it — the
// model-switch-to-a-smaller-window scenario — the harness drives the split
// instead of the single-shot compaction: the timeline is summarized in
// sequential chunks and committed ATOMICALLY as the new anchor. The legacy
// drain would only remove tool chains and keep the protected items over
// budget; the split shrinks the WHOLE timeline to fit the known window.
#[tokio::test]
async fn known_window_overflow_drives_split_and_commits_the_anchor() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    // The ACTIVE model's window is KNOWN (discovery succeeded) and far below
    // the held context: the single-shot compaction transcript would overflow
    // the provider, so the split path must fire instead.
    h = h.with_discovered_window(600);
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // A loaded user turn WITH an answer (promoted to a protected LoopClosure
    // below): protected-only over the trigger (~2200 ≥ 1600) and > 600 — the
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
        .with_mock_chat(Ok("## Objective\n- summarized chunk"))
        .with_mock_stream(Ok(vec!["final answer"]));

    // The harness (with its context manager) is moved into the task; the CM
    // state after the loop is reported back through this channel.
    let (state_tx, mut state_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<ContextItem>, usize)>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let info = h.context_manager.display_info();
        let _ = state_tx.send((h.context_manager.items_snapshot(), info.total_tokens));
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
    let (items, total_tokens) =
        tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
            .await
            .ok()
            .flatten()
            .unwrap_or((Vec::new(), 0));
    handle.abort();

    // The split surfaced the SAME continuous "Summarizing" box to the TUI.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started
            }
        )),
        "the split must emit LlmCompaction::Started; events={events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the split must emit LlmCompaction::Finished; events={events:?}"
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
        "the loop completes after the split; events={events:?}"
    );

    // The split committed ATOMICALLY: the whole timeline became a single
    // Compaction anchor (the legacy drain would have kept the protected items
    // over budget and marked the provider stuck instead).
    assert!(
        matches!(
            &items[0],
            ContextItem::Compaction { summary, .. }
                if summary.contains("summarized chunk")
        ),
        "the buffer is committed as the new anchor; items={items:?}"
    );
    assert!(
        !items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } | ContextItem::Assistant { original, .. }
                if original.starts_with("u ") || original.starts_with("a ")
        )),
        "the giant history was folded into the anchor — nothing stays verbatim"
    );
    // The re-added in-flight input and the loop's final answer follow it.
    assert!(
        matches!(
            &items[1],
            ContextItem::User { original, .. } if original == "hi"
        ),
        "the in-flight input is re-added after the split; items={items:?}"
    );
    assert!(
        matches!(
            &items[2],
            ContextItem::LoopClosure { content, .. } if content == "final answer"
        ),
        "the loop's final answer follows the anchor; items={items:?}"
    );
    // Contenção antecipada: the committed anchor fits the known window.
    assert!(
        total_tokens <= 600,
        "the anchor must fit the known window; total_tokens={total_tokens}"
    );
}

// The REACTIVE fork: when the single-shot compaction transcript itself
// overflows with a REPORTED window, `llm_compact` drives the split instead of
// the chain drain — the split shrinks the whole timeline (protected items
// included) into a fitting anchor and the compaction resolves successfully
// (Finished, never Failed; the provider is never marked stuck).
#[tokio::test]
async fn reactive_overflow_reports_window_and_drives_split_inside_llm_compact() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::core::CONTEXT_WINDOW_MARKER;
    use crate::harness::events::{LlmCompactionEvent, ToastVariant};

    let mut h = Harness::new_test();
    // NO discovered window: the loop-start PROACTIVE fork must NOT fire, so
    // `llm_compact` runs the single-shot compaction — whose transcript then
    // overflows WITH a reported window, triggering the reactive split.
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // Protected-only history over the trigger (~2200 ≥ 1600) and > 600 (the
    // window the error will report).
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("assistant".into(), "a ".repeat(1100)),
    ]);
    h.context_manager.close_loop();
    // Mock CHAT queue: call 1 = the single-shot compaction summarizer, which
    // fails with a ContextWindow that REPORTS the window (600); the rest =
    // the split chunks, which succeed. The timeline at split time holds THREE
    // items (the loaded history + the in-flight "hi" input); every chunk
    // boundary is forced because each pair of remaining items together
    // exceeds the tiny budget (window 600 − min(overhead, window/2) = 300)
    // — so three chunks follow the overflow.
    let overflow = format!("{CONTEXT_WINDOW_MARKER}:600");
    h = h
        .with_mock_chats(vec![
            Err(overflow.as_str()),
            Ok("## Objective\n- reactive chunk one"),
            Ok("## Objective\n- reactive chunk two"),
            Ok("## Objective\n- reactive chunk three"),
        ])
        .with_mock_stream(Ok(vec!["final answer"]));

    let (state_tx, mut state_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<ContextItem>, bool)>();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = tokio::spawn(async move {
        h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
            .await;
        let _ = state_tx.send((
            h.context_manager.items_snapshot(),
            h.context_manager.overflow_stuck("openai"),
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
    let (items, stuck) = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or((Vec::new(), false));
    handle.abort();

    // The single-shot overflow escalated into the split, which SUCCEEDED.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished
            }
        )),
        "the reactive split must finish successfully; events={events:?}"
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
    // The legacy drain was NOT taken: no persistent overflow warning toast.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            HarnessEvent::Toast {
                variant: ToastVariant::Warning,
                ..
            }
        )),
        "the split resolves the overflow without the stuck-provider warning; events={events:?}"
    );
    // The split chunks streamed to the TUI as one continuous box.
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::LlmCompactionToken { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        streamed.contains("reactive chunk one") && streamed.contains("reactive chunk three"),
        "every split chunk must stream; streamed={streamed:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop completes; events={events:?}"
    );

    // Atomic commit: the whole timeline became the concatenated anchor.
    assert!(
        matches!(
            &items[0],
            ContextItem::Compaction { summary, .. }
                if summary.contains("reactive chunk one")
                    && summary.contains("reactive chunk three")
        ),
        "the concatenated buffer is the new anchor; items={items:?}"
    );
    assert!(
        !items.iter().any(|it| matches!(
            it,
            ContextItem::User { original, .. } | ContextItem::Assistant { original, .. }
                if original.starts_with("u ") || original.starts_with("a ")
        )),
        "the giant history was folded into the anchor"
    );
    // Unlike the legacy drain, the reactive split resolves WITHOUT marking
    // the provider stuck.
    assert!(
        !stuck,
        "the reactive split must not mark the provider stuck"
    );
}

// All-or-nothing: an EMPTY chunk summary (the model returned nothing for a
// chunk) is treated as a split failure — the split aborts and the timeline
// stays exactly as it was. Nothing is silently dropped from the anchor.
#[tokio::test]
async fn split_aborts_on_an_empty_chunk_summary_and_keeps_the_context() {
    use crate::harness::context_manager::ContextManager;
    use crate::harness::events::LlmCompactionEvent;

    let mut h = Harness::new_test();
    h = h.with_discovered_window(600);
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // Protected-only history over the trigger AND over the known window.
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
    let items = tokio::time::timeout(tokio::time::Duration::from_secs(2), state_rx.recv())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    handle.abort();

    // The split was attempted and ABORTED — surfaced as a failed compaction.
    assert!(
        events.iter().any(|e| matches!(
            e,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Failed
            }
        )),
        "the empty-summary split must report LlmCompaction::Failed; events={events:?}"
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
        "the timeline keeps the original protected history; items={items:?}"
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
// The harness emits a serialized context snapshot per tool dispatch (with a
// test-zeroed cadence) so the TUI can write the `.ctx` companion file mid-run.
// This test proves the snapshots (1) arrive during the run, (2) deserialize
// into a valid [`ContextManagerState`], (3) reflect progress — each later
// snapshot owns the tool call/result accumulated since — and (4) restore into
// a fresh manager that keeps answering: exactly the round trip a crash/restart
// resume performs through the `.ctx` file.
#[tokio::test]
async fn run_agent_loop_emits_resumable_incremental_context_snapshots() {
    use crate::harness::context_manager::{ContextManager, ContextManagerState};

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

    let raw_snapshots: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::ContextSnapshot { context_state } => Some(context_state),
            _ => None,
        })
        .collect();
    assert!(
        raw_snapshots.len() >= 2,
        "a multi-dispatch run must snapshot per dispatch; got {}",
        raw_snapshots.len()
    );

    // Every snapshot is a valid bincode round trip of the persisted state.
    let states: Vec<ContextManagerState> = raw_snapshots
        .iter()
        .map(|raw| bincode::deserialize(raw).expect("a snapshot must deserialize"))
        .collect();

    // Progress: the snapshots accumulate the tool call/result pairs.
    let sizes: Vec<usize> = states.iter().map(|s| s.items.len()).collect();
    assert!(
        sizes.windows(2).all(|w| w[0] < w[1]),
        "each later snapshot must own strictly more accumulated context; sizes={sizes:?}"
    );

    // Resumability: restore the LAST snapshot into a fresh manager and keep
    // answering — the exact `.ctx` round trip on restart.
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
