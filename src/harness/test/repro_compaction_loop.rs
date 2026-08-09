use super::super::core::Harness;
use super::super::events::HarnessEvent;
use crate::harness::context_manager::ContextManager;
use crate::harness::events::LlmCompactionEvent;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

// ── Reproducer: the "infinite summarization" feedback loop ────────────────
//
// User report (BUG): when the budget hits the ceiling and every deterministic
// resource is spent, the LLM compaction runs and the summarizer works fine —
// but the summarization keeps re-firing over and over ("loop infinito a
// sumerização do agent"), with the old context appearing to be folded back in
// each time. The model window is far larger than the trigger, so the summary
// never approaches the budget.
//
// Mechanism this test pins down: a single `run_agent_loop` can fire the LLM
// compaction MULTIPLE times. Tool chains are PROTECTED content (never
// prose-compressed, never evicted), so once the model adds a big tool call the
// deterministic phases resolve nothing and `run()` returns
// `NeedsLlmCompaction` again after EVERY dispatch that pushes the total back
// over the 80% trigger. Each compaction serializes the ENTIRE accumulated
// timeline (the previous summary + everything since — the "old context") into
// the summarizer prompt. With a hardcoded budget far below the model window,
// the trigger re-fires constantly and the summarizer is called once per
// overflow — the loop the user reported.
#[tokio::test]
async fn repro_llm_compaction_repeats_mid_loop_when_protected_content_regrows() {
    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(2000); // trigger = 1600
    // Protected-only history over the trigger: no drafts for the pipeline or
    // the eviction pass → NeedsLlmCompaction at loop start.
    h = h.with_history(&[
        ("user".into(), "u ".repeat(1100)),
        ("user".into(), "v ".repeat(1100)),
    ]);
    h = h.with_test_tool(
        "test_tool",
        serde_json::json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "required": ["x"]
        }),
    );
    // The summarizer always succeeds with a small summary.
    h = h.with_mock_chat(Ok("## Objective\n- compacted"));
    // Each work iteration adds a HUGE PROTECTED tool chain (the call
    // arguments are never evictable / compressible), pushing the total back
    // over the trigger → the LLM compaction re-fires after EVERY dispatch.
    let huge = "a".repeat(20_000);
    h.mock_stream_queue.push_back(Ok(vec![
        "Let me inspect ".to_string(),
        format!(r#"{{"name": "test_tool", "arguments": {{"x": "{huge}"}}}}"#),
    ]));
    h.mock_stream_queue.push_back(Ok(vec![
        "Let me inspect again ".to_string(),
        format!(r#"{{"name": "test_tool", "arguments": {{"x": "{huge}"}}}}"#),
    ]));
    h.mock_stream_queue.push_back(Ok(vec!["done".to_string()]));

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

    let started = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Started
                }
            )
        })
        .count();
    let finished = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Finished
                }
            )
        })
        .count();

    // One compaction at loop start + one per mid-loop overflow. Two big tool
    // chains → at least 3 summarizer calls inside ONE agent loop.
    assert!(
        started >= 3,
        "the LLM compaction must re-fire after each mid-loop overflow, got \
         {started} Started events; events={events:?}"
    );
    assert_eq!(
        started, finished,
        "every started compaction must finish; events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the loop still completes (bounded by MAX_ITERATIONS); events={events:?}"
    );
}
