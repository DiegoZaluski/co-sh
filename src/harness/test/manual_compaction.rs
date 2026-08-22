//! Tests for the user-triggered `/compact`
//! ([`Harness::compact_on_demand`]): the deterministic funnel runs across
//! every segment even far below the 80% trigger, then the LLM summary folds
//! the remainder — reusing the automatic compaction's events end to end.

use super::super::core::Harness;
use crate::harness::context_manager::{ContextManager, RunOutcome};
use crate::harness::core::ManualCompactionOutcome;
use crate::harness::events::{HarnessEvent, LlmCompactionEvent};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn compact_on_demand_grinds_then_summarizes_below_the_trigger() {
    let mut h = Harness::new_test();
    h.context_manager = ContextManager::new(200_000); // normal trigger = 160k
    h.context_manager.add_user("explore the repo");
    for i in 0..3 {
        h.context_manager
            .add_assistant(&format!("finding {i}: {}", "detail ".repeat(400)), true);
    }
    assert!(
        h.context_manager.display_info().total_tokens < 160_000,
        "precondition: under the normal trigger, so a plain run does nothing"
    );
    assert_eq!(h.context_manager.run(), RunOutcome::Resolved);

    h = h.with_mock_chat(Ok("## Objective\n- compacted"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let outcome = h.compact_on_demand(&tx, stop_signal).await;

    assert_eq!(outcome, ManualCompactionOutcome::Compacted);
    assert_eq!(
        h.context_manager.items_snapshot().len(),
        1,
        "the timeline folds into the single summary"
    );

    // The automatic path's lifecycle reached the TUI: Started + Finished and
    // a persisted snapshot.
    let mut started = false;
    let mut finished = false;
    let mut snapshot = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Started,
            } => started = true,
            HarnessEvent::LlmCompaction {
                event: LlmCompactionEvent::Finished,
            } => finished = true,
            HarnessEvent::ContextSnapshot { .. } => snapshot = true,
            _ => {}
        }
    }
    assert!(started, "the Summarizing box must open");
    assert!(finished, "the Summarizing box must close cleanly");
    assert!(
        snapshot,
        "the compacted context must be persisted via ContextSnapshot"
    );

    // The manual flag never leaks: a later run behaves normally.
    assert!(!matches!(
        h.context_manager.run(),
        RunOutcome::NeedsLlmCompaction
    ));
}

/// A session whose timeline holds only the previous summary reports it
/// without emitting any event or burning a model call.
#[tokio::test]
async fn compact_on_demand_reports_nothing_to_compact() {
    let mut h = Harness::new_test();
    let _ = h
        .context_manager
        .apply_llm_summary("previous anchor".into());
    h = h.with_mock_chat(Ok("## Objective\n- should never be called"));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let outcome = h.compact_on_demand(&tx, stop_signal).await;

    assert_eq!(outcome, ManualCompactionOutcome::NothingToCompact);
    assert!(rx.try_recv().is_err(), "no events on the noop path");
}
