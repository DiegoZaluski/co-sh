//! Bug-hunt reproducer tests.
//!
//! Each test in this file deterministically reproduces a subtle harness bug
//! that was confirmed by code analysis. The assertions encode the **expected**
//! (correct) behavior; under the current implementation they FAIL, proving the
//! bug exists. See `.dev-notes/BUG-2026-07-31.md` for the full report.
//!
//! Bugs covered:
//! - BUG-01: user input is duplicated in the first LLM request
//! - BUG-02: loaded history turns were wiped/undercounted by the transcript
//!   budget (fixed structurally: the ContextManager is the single owner)
//! - BUG-03: extraction-failure retry loop has no MAX_ITERATIONS guard
//! - BUG-04: `stop_agent_loop` silently drops pending tool calls
//! - BUG-05: `ask_questions` answer wait is not interruptible by stop_signal
//! - BUG-06: fallback connector switch loses native tool definitions
//! - BUG-07: full conversation is sent twice per request (system + messages)

use super::super::core::{Harness, MAX_ITERATIONS};
use super::super::events::HarnessEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Run the agent loop to completion (it must terminate) and return the harness
/// plus all events that were emitted. Events are drained from the unbounded
/// channel after the loop returns.
async fn run_loop_and_collect(mut h: Harness, input: &str) -> (Harness, Vec<HarnessEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    h.run_agent_loop(input, tx, answer_rx, perm_rx, stop_signal)
        .await;

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    (h, events)
}

// ── BUG-01 ──────────────────────────────────────────────────────────
// The user input used to be pushed into `history` AND appended again as
// `current_input`, so the message was sent twice (or three times, since the
// TUI already includes it in the `with_history` turns). The context manager
// now owns the input once; `build_messages` appends the steering input only
// when it differs from the trailing user turn.
#[test]
fn bug01_user_input_is_duplicated_in_first_request() {
    let mut h = Harness::new_test().with_history(&[("user".into(), "hello there".into())]);

    // This mirrors the exact state run_agent_loop creates before iteration 1:
    // history contains the user turn, and current_input == same text.
    let msgs = h.build_messages_for_test("hello there");

    let count = msgs
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref() == Some("hello there"))
        .count();

    assert_eq!(
        count, 1,
        "BUG-01: user input must be sent exactly once, but was sent {count} times in the first request"
    );
}

// ── BUG-02 (structurally fixed) ─────────────────────────────────────────
// The old bug: run_agent_loop assigned `total_history_tokens` instead of
// accumulating it, wiping the count built by with_history() so the transcript
// budget never fired for loaded turns. The transcript counter is gone
// entirely — the ContextManager is the single owner of the conversation, so
// loaded turns cannot be "wiped": they live in the CM and are delivered via
// the messages array on every iteration.
#[tokio::test]
async fn bug02_history_turns_survive_the_loop_in_the_context_manager() {
    let marker = "LOADED_TURN_MARKER_XYZ";
    let h = Harness::new_test()
        .with_history(&[
            ("user".into(), marker.into()),
            // The loaded turn is a COMPLETED one (it has an answer): the next
            // input lands after an output, never directly on a user turn (that
            // pattern is the abandoned-input case, which the harness drops).
            ("assistant".into(), "loaded reply".into()),
        ])
        .with_mock_stream(Ok(vec!["final answer"]));

    let (mut h, _events) = run_loop_and_collect(h, "hi").await;

    // The loaded turn is still owned by the CM (single owner) after the loop
    // and is delivered in the messages payload — nothing was wiped.
    assert!(
        h.context_manager.display_info().total_tokens > 0,
        "loaded history must remain in the context manager after the loop"
    );
    let msgs = h.build_messages_for_test("");
    let joined: String = msgs
        .iter()
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains(marker),
        "loaded history must still be delivered via the messages array"
    );
}

// ── BUG-03 ──────────────────────────────────────────────────────────
// The MAX_ITERATIONS guard only lives inside `while self.has_pending_tools()`.
// The extraction-failure retry path (`!had_tools && extraction_failures > 0`)
// loops unconditionally, so a model emitting invalid tool JSON spins forever
// (each iteration = one more LLM call). The retry must be bounded by
// MAX_ITERATIONS.
#[tokio::test]
async fn bug03_extraction_failure_retry_has_no_iteration_cap() {
    let invalid = r#"{"name": "no_such_tool", "arguments": {}}"#;
    // Queue enough invalid responses to cross the cap.
    let responses: Vec<Result<Vec<&str>, &str>> =
        (0..MAX_ITERATIONS + 5).map(|_| Ok(vec![invalid])).collect();

    let h = Harness::new_test().with_mock_streams(responses);

    let (_h, events) = run_loop_and_collect(h, "do something").await;

    // Each retry iteration streams the "⚠ Tool call failure" message once.
    let failure_tokens = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Token { text } if text.contains("Tool call failure")))
        .count();

    assert!(
        failure_tokens <= MAX_ITERATIONS as usize,
        "BUG-03: retry loop must stop at MAX_ITERATIONS ({MAX_ITERATIONS}), but ran {failure_tokens} iterations"
    );
}

// ── BUG-04 ──────────────────────────────────────────────────────────
// When the model emits a tool call and then `stop_agent_loop` in the same
// response, handle_harness_tool sets `self.stop = true` immediately. The
// check_stop!() after phase 1 breaks the loop BEFORE the queued tool call is
// dispatched — mid-work work is silently dropped (matches "loop stopped for no
// reason").
#[tokio::test]
async fn bug04_stop_agent_loop_drops_pending_tool_calls() {
    let h = Harness::new_test()
        .with_test_tool(
            "my_tool",
            serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}),
        )
        .with_mock_stream(Ok(vec![
            r#"{"name": "my_tool", "arguments": {"x": "a"}}"#,
            r#"{"name": "stop_agent_loop", "arguments": {}}"#,
        ]));

    let (h, events) = run_loop_and_collect(h, "do the thing").await;

    // The queued tool call must have been PROCESSED (dispatched), not silently
    // dropped. `my_tool` has no dispatch target in new_test(), so a correct
    // implementation emits ToolError ("no server found") — we accept either a
    // result OR an error, since both prove dispatch was attempted.
    assert!(
        events.iter().any(|e| {
            matches!(
                e,
                HarnessEvent::ToolResult { .. } | HarnessEvent::ToolError { .. }
            )
        }),
        "BUG-04: pending tool call was never dispatched — silently dropped by stop_agent_loop"
    );
    assert!(
        !h.has_pending_tools(),
        "BUG-04: tool_issuer still holds undelivered tool calls after the loop"
    );
}

// ── BUG-05 ──────────────────────────────────────────────────────────
// The permission wait uses tokio::select! with a stop_signal poll, but the
// ask_questions answer wait is a bare `answer_rx.recv().await` — it cannot be
// interrupted. If the TUI never answers, the loop hangs forever with the
// spinner active.
#[tokio::test]
async fn bug05_question_answer_wait_ignores_stop_signal() {
    let mut h = Harness::new_test()
        .with_test_tool(
            "ask_questions",
            serde_json::json!({"type": "object", "properties": {"questions": {"type": "array"}}, "required": ["questions"]}),
        )
        .with_mock_stream(Ok(vec![
            r#"{"name": "ask_questions", "arguments": {"questions": [{"id": "q1", "question": "Q?", "type": "Text"}]}}"#,
        ]));

    let (tx, mut rx) = mpsc::unbounded_channel();
    // The TUI keeps the answer sender alive for the whole session
    // (App::answer_tx is stored in app state) — only the loop's own
    // stop_signal can interrupt the wait.
    let (_answer_tx, answer_rx) = mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let stop_for_task = stop_signal.clone();
    let mut handle = tokio::spawn(async move {
        h.run_agent_loop("ask me", tx, answer_rx, perm_rx, stop_for_task)
            .await;
    });

    // Wait for the loop to emit the QuestionRequest (i.e. it is now blocked
    // inside answer_rx.recv()).
    let mut got_question = false;
    for _ in 0..50 {
        if let Ok(Some(HarnessEvent::QuestionRequest { .. })) =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
        {
            got_question = true;
            break;
        }
    }
    assert!(got_question, "loop should have requested questions");

    // User interrupts: stop_signal is set. The answer wait is a bare
    // answer_rx.recv().await with no stop check, so the loop keeps blocking.
    stop_signal.store(true, Ordering::Relaxed);

    // A correct implementation terminates promptly after stop_signal; the
    // buggy one stays stuck until the timeout elapses.
    let finished = tokio::time::timeout(std::time::Duration::from_millis(400), &mut handle).await;

    // If the loop is stuck, terminate the task so it does not leak — abort
    // before the assert so cleanup happens even if the assert panics.
    handle.abort();

    assert!(
        finished.is_ok(),
        "BUG-05: agent loop must terminate within 400ms after stop_signal, but hung waiting for question answers"
    );
}

// ── BUG-07 (structurally fixed) ────────────────────────────────────────
// The old bug: build_chat_context() appended the ENTIRE ContextManager buffer
// to the system prompt while the messages array sent the same turns again —
// every turn travelled twice per request. The system prompt is now
// conversation-free: the whole conversation lives in the context manager and
// is delivered exactly once, via the messages array.
#[test]
fn bug07_full_conversation_sent_twice_system_and_messages() {
    let marker = "UNIQUE_HISTORY_MARKER_XYZ";

    let mut h = Harness::new_test().with_history(&[("user".into(), marker.into())]);

    let system_ctx = h.build_chat_context_for_test();
    let msgs = h.build_messages_for_test("");
    let joined: String = msgs
        .iter()
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    // Correct behavior: the same turn must NOT appear in both the system
    // context and the messages array (it is sent twice per request).
    let in_system = system_ctx.contains(marker);
    let in_messages = joined.contains(marker);
    assert!(
        !(in_system && in_messages),
        "BUG-07: conversation turn sent twice — system_ctx={in_system}, messages={in_messages}"
    );
}

// ── BUG-08 (structurally fixed) ────────────────────────────────────────
// The old bug: the over-budget path pushed the FULL raw buffer as a
// "## Compressed Prior Context" system message, so payloads grew unboundedly.
// The transcript budget is gone; the ContextManager is the single owner and
// compacts with its 80/40 phases. The messages array never contains a
// raw-buffer/truncation system message.
#[test]
fn bug08_over_budget_system_message_ships_full_raw_buffer() {
    let marker = "UNIQUE_RAW_BUFFER_MARKER_XYZ";
    // ~55k tokens — above HISTORY_BUDGET (50_000)
    let big = format!("{marker} ") + &"word ".repeat(55_000);

    let mut h = Harness::new_test().with_history(&[("user".into(), big.clone())]);

    let msgs = h.build_messages_for_test("hi");
    let system_text: String = msgs
        .iter()
        .filter(|m| m.role == "system")
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    // Correct behavior: the over-budget system message must contain a
    // COMPRESSED summary — never the raw buffer verbatim.
    assert!(
        !system_text.contains(marker),
        "BUG-08: 'Compressed Prior Context' system message contains the FULL raw buffer ({marker}) — the payload grows unboundedly every iteration"
    );
}

// ── BUG-06 ──────────────────────────────────────────────────────────
// init_native_tools() runs once at loop start. When a connector error triggers
// a fallback switch, `self.connector = c.with_model(...)` replaces the
// connector — but native tool definitions are NOT re-registered on the new one.
// The fallback provider therefore receives zero tool definitions.
#[tokio::test]
async fn bug06_fallback_switch_loses_native_tools() {
    // new_test() starts with provider "openai"; the fallback is "claude" so
    // this assertion only holds if the fallback switch actually ran.
    let h = Harness::new_test()
        .with_mock_stream(Err("connector down"))
        .with_fallbacks(vec![("claude".into(), "claude-sonnet-4-5".into())]);

    let (h, _events) = run_loop_and_collect(h, "hello").await;

    assert_eq!(
        h.connector_provider(),
        Some("claude"),
        "fallback connector should have been selected"
    );
    assert!(
        h.connector_has_tools(),
        "BUG-06: fallback connector has NO native tool definitions (init_native_tools not re-run after switch)"
    );
}

// ── BUG-09 ──────────────────────────────────────────────────────────
// When the dispatch guards fire (MAX_ITERATIONS or MAX_TOOL_RETRIES), a
// terminal event (Done/Error) is emitted INSIDE the dispatch loop. The
// `check_stop!()` right after the loop used to emit a SECOND terminal event
// (Stopped) because it set self.stop — the TUI received Done+Stopped (or
// Error+Stopped), showing a spurious "Interrupted" toast after a normal
// completion and double-saving the session.
#[tokio::test]
async fn bug09_dispatch_guard_emits_single_terminal_event() {
    // MAX_TOOL_RETRIES path: a tool call that always fails dispatch (no server
    // found in new_test) hits the 3-strikes guard.
    let h = Harness::new_test()
        .with_test_tool(
            "always_fails",
            serde_json::json!({"type": "object", "properties": {}}),
        )
        .with_mock_streams(vec![
            Ok(vec![r#"{"name": "always_fails", "arguments": {}}"#]),
            Ok(vec![r#"{"name": "always_fails", "arguments": {}}"#]),
            Ok(vec![r#"{"name": "always_fails", "arguments": {}}"#]),
        ]);

    let (_h, events) = run_loop_and_collect(h, "do the thing").await;

    let errors = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Error { .. }))
        .count();
    let stopped = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Stopped { .. }))
        .count();

    assert_eq!(
        errors, 1,
        "BUG-09: exactly one Error from the retry guard, got {errors}"
    );
    assert_eq!(
        stopped, 0,
        "BUG-09: retry guard must NOT emit a second terminal Stopped event"
    );
}

// ── BUG-10 ──────────────────────────────────────────────────────────
// The provider can cut the stream at max_tokens with finish_reason = "length"
// (e.g. Claude's default 4096 output tokens). The harness used to treat this
// as a normal completion (Done) even though the answer is truncated mid-word —
// the loop silently ended mid-work. Correct behaviour: continue the loop with
// a continuation prompt so the model can finish the response.
#[tokio::test]
async fn bug10_length_truncation_continues_loop_instead_of_done() {
    let h = Harness::new_test()
        .with_mock_finish_reason(Some("length"))
        .with_mock_streams(vec![
            Ok(vec!["partial answer that got cut off"]),
            Ok(vec!["completed answer"]),
        ]);

    let (_h, events) = run_loop_and_collect(h, "write a long thing").await;

    // The loop must have continued past the truncated first stream (two
    // streams consumed) instead of emitting Done after the first one.
    let done = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Done { .. }))
        .count();
    let tokens = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Token { .. }))
        .count();

    assert_eq!(
        done, 1,
        "BUG-10: truncation must not emit Done before the continuation stream"
    );
    assert!(
        tokens >= 2,
        "BUG-10: continuation stream must have been consumed (got {tokens} tokens)"
    );
}

// ── BUG-11 ──────────────────────────────────────────────────────────
// Same as BUG-09 but for the MAX_ITERATIONS guard inside the dispatch loop:
// reaching the iteration cap with pending tools must emit exactly one Done —
// no spurious Stopped after it.
//
// A tool that always fails dispatch triggers MAX_TOOL_RETRIES (3) first, so
// this exercises the MAX_ITERATIONS safety net through the extraction-failure
// retry path instead: the model keeps emitting invalid tool JSON, the loop
// retries bounded by MAX_ITERATIONS, and the cap emits a single Done.
#[tokio::test]
async fn bug11_max_iterations_emits_single_done() {
    let invalid = r#"{"name": "no_such_tool", "arguments": {}}"#;
    let responses: Vec<Result<Vec<&str>, &str>> =
        (0..MAX_ITERATIONS + 2).map(|_| Ok(vec![invalid])).collect();

    let h = Harness::new_test().with_mock_streams(responses);

    let (_h, events) = run_loop_and_collect(h, "do the thing").await;

    let done = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Done { .. }))
        .count();
    let stopped = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::Stopped { .. }))
        .count();

    assert_eq!(
        done, 1,
        "BUG-11: MAX_ITERATIONS guard emits exactly one Done"
    );
    assert_eq!(
        stopped, 0,
        "BUG-11: MAX_ITERATIONS guard must NOT emit a second Stopped event"
    );
}
