//! Timing probes for the agent-loop hot path.
//!
//! These tests measure the wall-clock cost of every **non-network** operation
//! the harness performs between tool calls (message building, header cloning,
//! stream extraction, context compaction, tool dispatch) and prove two things:
//!
//! 1. The harness's own per-iteration work is **milliseconds** even with a
//!    large conversation and a 100k-token budget — local compute is NOT the
//!    30s–1min "between tool calls" the user observes.
//! 2. The dominant wall-clock between tool calls is the provider round-trip
//!    (prefill) over a conversation that grows to ~80% of a 100k-token budget
//!    before anything is trimmed, with the FULL payload + the system header
//!    (instructions + every tool schema, pretty-printed) re-sent on **every**
//!    loop iteration.
//!
//! Two behaviors here are correct-by-design and are asserted only as facts:
//! write/execute tools gate on a user permission dialog (the loop blocks until
//! the human answers — the intended contract), and multiple tool calls from
//! one response are dispatched sequentially (preserves the agent's intent).
//! Neither is a defect; both are measured only to characterise the wall clock.

use crate::harness::context_manager::{ContextManager, MAX_CONTEXT_TOKENS};
use crate::harness::core::{Harness, Mode, StreamEvent};
use crate::harness::guardrails::{PermissionCheck, check_tool_permission};
use crate::util::estimate_tokens;
use cosh_sdk::connector::Connector;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Helper: fill a context manager with `rounds` realistic tool-call rounds
/// (user → assistant → tool call → tool result) and return the byte size of
/// the payload `build_messages` renders (what the provider receives).
fn fill_tool_rounds(cm: &mut ContextManager, rounds: usize) -> usize {
    let mut payload_bytes = 0usize;
    for i in 0..rounds {
        cm.add_user("Continue with the next step of the task.");
        cm.add_assistant(
            "I will inspect the relevant files and apply a targeted change.",
            true,
        );
        cm.add_tool_call(
            &format!("call_{i}"),
            "fs_edit",
            r#"{"targets":[{"path":"src/lib.rs","replacements":[]}]}"#,
        );
        let body = format!(
            "-- round {i} result --\n{}",
            "let value = compute(input);\nvalue.map(|x| format!(\"{x:?}\"))\n".repeat(20)
        );
        cm.add_tool_result(&format!("call_{i}"), &body);
        let msgs = cm.build_messages("");
        payload_bytes = msgs
            .iter()
            .map(|m| m.role.len() + m.content.as_deref().unwrap_or("").len())
            .sum();
    }
    payload_bytes
}

/// PROOF 1 — the harness's own per-iteration work is milliseconds, even with a
/// large conversation. If the 30s–1min "between tool calls" were local compute,
/// this test would show it. It does not: the whole local loop (message
/// rendering + compaction tick + budget info) stays well under 1 second.
#[tokio::test]
async fn per_iteration_local_work_is_milliseconds_even_with_large_conversation() {
    let mut cm = ContextManager::new(MAX_CONTEXT_TOKENS);
    let payload_bytes = fill_tool_rounds(&mut cm, 25);
    let total = cm.display_info().total_tokens;
    assert!(
        total > 1_000,
        "expected a substantial conversation, got {total} estimated tokens"
    );
    eprintln!(
        "[perf] conversation: {total} estimated tokens, rendered payload ≈ {payload_bytes} bytes"
    );

    let start = std::time::Instant::now();
    let msgs = cm.build_messages("");
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = std::time::Instant::now();
    cm.run();
    let run_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = std::time::Instant::now();
    let _info = cm.display_info();
    let info_ms = start.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "[perf] build_messages({} msgs): {build_ms:.3} ms | run(): {run_ms:.3} ms | display_info(): {info_ms:.3} ms",
        msgs.len()
    );
    assert!(build_ms < 500.0, "build_messages took {build_ms:.1} ms");
    assert!(run_ms < 500.0, "context_manager.run() took {run_ms:.1} ms");
    assert!(info_ms < 200.0, "display_info() took {info_ms:.1} ms");
}

/// PROOF 2 — streaming + per-token extraction of a 2000-token response is fast.
/// The per-chunk `tokio::select!` + 50ms stop-poll machinery does not add any
/// meaningful latency to a prompt provider (chunks are processed the moment
/// the stream yields them).
#[tokio::test]
async fn streaming_and_extraction_of_many_tokens_is_fast() {
    let tokens: Vec<&str> = std::iter::repeat_n("token ", 2000).collect();
    let mut h = Harness::new_test().with_mock_stream(Ok(tokens));
    let start = std::time::Instant::now();
    let mut received = 0usize;
    let result = h
        .stream_chat_with_messages("sys", &[], |e| {
            if let StreamEvent::Token(t) = e {
                received += t.len();
            }
        })
        .await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(result.as_deref(), Ok("done"));
    assert!(received > 0, "expected tokens to be delivered");
    eprintln!("[perf] 2000-token stream through the extractor: {elapsed_ms:.3} ms");
    assert!(
        elapsed_ms < 2000.0,
        "2000-token stream took {elapsed_ms:.1} ms"
    );
}

/// PROOF 3 — with the default 100k budget, the conversation is held in full
/// (nothing trimmed) until ~80% (80k estimated tokens), and every loop
/// iteration re-serializes the ENTIRE conversation for the provider. That is
/// the dominant per-iteration cost driver: provider prefill on a prompt that
/// only grows. A small budget compacts promptly — proving the phases work, but
/// only fire extremely late at the default budget.
#[tokio::test]
async fn default_budget_holds_entire_conversation_until_80_percent() {
    // (a) Default budget: fill past 50k estimated tokens — run() must NOT trim.
    let mut cm = ContextManager::new(MAX_CONTEXT_TOKENS);
    let mut payload_bytes = 0usize;
    let mut rounds = 0usize;
    while cm.display_info().total_tokens < MAX_CONTEXT_TOKENS / 2 {
        cm.add_user("Continue.");
        cm.add_assistant("Working on it.", true);
        cm.add_tool_call(
            &format!("c{rounds}"),
            "find_glob",
            r#"{"pattern":"**/*.rs","path":"src"}"#,
        );
        cm.add_tool_result(&format!("c{rounds}"), &"let x = 1;\n".repeat(40));
        rounds += 1;
        payload_bytes = cm
            .build_messages("")
            .iter()
            .map(|m| m.role.len() + m.content.as_deref().unwrap_or("").len())
            .sum();
        if rounds > 10_000 {
            break;
        }
    }
    let before = cm.display_info();
    let len_before = cm.items_snapshot().len();
    cm.run();
    let len_after = cm.items_snapshot().len();
    assert_eq!(
        len_before, len_after,
        "default 100k budget must NOT trim at {} estimated tokens",
        before.total_tokens
    );
    eprintln!(
        "[perf] {rounds} tool rounds held: {} estimated tokens, rendered payload ≈ {payload_bytes} bytes — the FULL payload is re-sent to the provider on EVERY loop iteration",
        before.total_tokens
    );

    // (b) A small budget compacts at 80% → back just under the 80% trigger
    // (the minimum decompaction): the pipeline summarizes the compressible
    // assistant drafts in place (user prompts are protected and never
    // touched). The phases work, they just trigger very late at the default
    // 100k budget.
    let mut small = ContextManager::new(4_000);
    for i in 0..20 {
        small.add_user("Continue.");
        small.add_assistant(
            &format!(
                "Round {i}: {}",
                "the quick brown fox jumps over the lazy dog near the riverbank and the mountain trail. "
                    .repeat(20)
            ),
            true,
        );
    }
    let small_before = small.display_info();
    assert!(
        small_before.total_tokens >= 3_200,
        "precondition: small budget is over the 80% trigger"
    );
    small.run();
    let small_after = small.display_info();
    eprintln!(
        "[perf] small budget: {}/4000 before run() → {} after (trigger at 3200)",
        small_before.total_tokens, small_after.total_tokens
    );
    assert!(
        small_after.total_tokens < 3_200,
        "expected compaction back under the 80% trigger, got {}",
        small_after.total_tokens
    );
}

/// PROOF 4 — the system header (instructions + all tool schemas) is cloned and
/// re-sent as the system message on every iteration. Even without MCP servers,
/// it is tens of KB; the clone itself is cheap, but the provider re-prefills it
/// every iteration alongside the full conversation.
#[tokio::test]
async fn system_header_is_resent_every_iteration() {
    let connector = Connector::new("openai").expect("openai provider config");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    eprintln!(
        "[perf] system header (instructions + all tool schemas): {} bytes ≈ {} estimated tokens",
        header.len(),
        estimate_tokens(&header)
    );

    // Per-iteration clone cost (the `build_chat_context` call in the loop).
    let start = std::time::Instant::now();
    let mut total = 0usize;
    for _ in 0..100 {
        total += h.build_chat_context_for_test().len();
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[perf] 100 header clones: {ms:.3} ms (total {total} bytes)");
    assert!(ms < 500.0, "100 header clones took {ms:.1} ms");
}

/// FACT (correct-by-design) — write/execute tools always gate the agent loop
/// on the user: in Build mode `check_tool_permission` returns NeedsApproval
/// for these tools even with in-root relative paths, and `run_agent_loop`
/// blocks on `perm_rx.recv()` until the human answers. This is the intended
/// permission contract, not a defect; the wall-clock per call includes the
/// user's reaction time.
#[test]
fn write_and_execute_tools_always_gate_on_user_permission() {
    let root = std::path::Path::new("/tmp/project-root");
    let always_approval: &[(&str, serde_json::Value)] = &[
        ("bash_run", serde_json::json!({"command": "cargo test"})),
        (
            "fs_write",
            serde_json::json!({"targets": [{"path": "src/main.rs", "content": "x"}]}),
        ),
        (
            "fs_edit",
            serde_json::json!({"targets": [{"path": "src/main.rs"}]}),
        ),
        (
            "fs_rollback",
            serde_json::json!({"path": "src/main.rs", "hash": "abc"}),
        ),
        (
            "subagent_call",
            serde_json::json!({"agent": "opencode", "input": "hi"}),
        ),
    ];
    for (tool, args) in always_approval {
        let check = check_tool_permission(tool, args, Mode::Build, Some(root));
        assert!(
            matches!(check, PermissionCheck::NeedsApproval(_)),
            "{tool} must ask the user for approval in Build mode, got {check:?}"
        );
    }

    // Read-only, in-root relative paths pass through without a dialog — so the
    // "simple" reads are not human-gated, but every write/exec is.
    let read = check_tool_permission(
        "fs_read",
        &serde_json::json!({"targets": [{"path": "src/lib.rs"}]}),
        Mode::Build,
        Some(root),
    );
    assert!(
        matches!(read, PermissionCheck::Allowed),
        "in-root relative fs_read must not need approval, got {read:?}"
    );
}

/// PROOF 6 — the loop machinery itself adds nothing: a full 6-iteration agent
/// loop with an instant (mock) provider completes in well under 2s. Therefore
/// the 30s–1min per tool call the user sees cannot come from the harness's
/// synchronous work — it comes from the provider round-trip (prefill on a
/// growing 100k-token context) and from human-gated permission dialogs.
#[tokio::test]
async fn full_agent_loop_with_fast_provider_is_fast() {
    let mut h = Harness::new_test()
        .with_mock_streams(vec![Ok(vec!["partial "]); 5])
        .with_mock_finish_reason(Some("length"))
        .with_mock_finish_reason(Some("length"))
        .with_mock_finish_reason(Some("length"))
        .with_mock_finish_reason(Some("length"))
        .with_mock_finish_reason(Some("length"))
        .with_mock_stream(Ok(vec!["final answer"]))
        .with_mock_finish_reason(Some("stop"));

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let start = std::time::Instant::now();
    h.run_agent_loop("do the work", tx, answer_rx, perm_rx, stop_signal)
        .await;
    let elapsed = start.elapsed();
    eprintln!("[perf] 6-iteration agent loop (instant provider): {elapsed:?}");
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "the harness loop machinery itself took {elapsed:?}"
    );
}
