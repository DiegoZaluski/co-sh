//! An IMPROPER stop — a loop that stopped with NOBODY asking it to — must not
//! end a turn that owes a REPORT.
//!
//! The field defect this pins: a `/agents` cosh pane ended its turn right
//! after a reasoning, its requester received "the pane produced no report",
//! and the turn closed `incomplete` while the model was one attempt away from
//! delivering it. For such a loop the only legitimate early end is the USER's
//! stop (`stop_agent_loop` is blocked for panes, so the loop cannot stop
//! itself), which is exactly observable — so continuing is not a judgement
//! call, and the re-send is the same history-plus-steering mechanism every
//! other continuation in the loop already uses.

use super::super::core::Harness;
use super::super::core::ResumeCause;
use super::super::events::HarnessEvent;
use crate::harness::context::ContextItem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Drive a loop to completion, returning it plus everything it emitted.
async fn drive(mut h: Harness) -> (Harness, Vec<HarnessEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    h.run_agent_loop(
        "do the delegated work",
        tx,
        answer_rx,
        perm_rx,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let mut events = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(tokio::time::Duration::from_millis(200), rx.recv()).await
    {
        events.push(event);
    }
    (h, events)
}

/// A cosh pane: a loop bound to its own registry id — the shape whose stop
/// hands a blocked `agents_call` requester an `incomplete` report.
fn pane() -> Harness {
    Harness::new_test().with_own_pane_id(Some("cosh".into()))
}

fn has_toast(events: &[HarnessEvent], needle: &str) -> bool {
    events.iter().any(
        |e| matches!(e, HarnessEvent::Toast { message, .. } if message.contains(needle)),
    )
}

/// THE REGRESSION: the natural-stop budget is spent and the model returns
/// nothing. A chat turn ends there (nothing is owed); a PANE owes the report
/// its requester is blocked on, so it must re-send the context instead of
/// closing `incomplete`.
#[tokio::test]
async fn a_pane_resumes_an_exhausted_empty_stop_instead_of_closing_incomplete() {
    let mut h = pane().with_mock_streams(vec![Ok(vec![]), Ok(vec!["the ", "report"])]);
    // The budget already spent: the FIRST empty stream reaches the exhausted
    // branch — the exact shape that used to end the turn.
    h.set_stop_nudges_left_for_test(0);

    let (h, events) = drive(h).await;

    let items: Vec<ContextItem> = h.context_manager.items_snapshot();
    match items.last() {
        Some(ContextItem::Closure { content, .. }) => assert_eq!(content, "the report"),
        other => panic!("the pane must resume and deliver its report, got {other:?}"),
    }
    assert!(
        !has_toast(&events, "empty response repeatedly"),
        "an improper stop must not degrade into the 'ends without an answer' toast: {events:?}"
    );
    assert_eq!(
        h.pane_resumes_left(),
        Harness::pane_resume_budget() - 1,
        "the resume must consume exactly one unit of the bound"
    );
}

/// A CHAT turn keeps its old behavior: its stop is final (the user reads the
/// answer, or asks again), so no resume and no fabricated continuation.
#[tokio::test]
async fn a_chat_loop_does_not_resume_an_exhausted_empty_stop() {
    let mut h = Harness::new_test().with_mock_streams(vec![Ok(vec![]), Ok(vec!["unused"])]);
    h.set_stop_nudges_left_for_test(0);

    let (h, events) = drive(h).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, HarnessEvent::Done { .. })),
        "the chat turn must end with Done: {events:?}"
    );
    assert_eq!(
        h.context_manager.final_answer(),
        None,
        "an exhausted empty stop records no Closure"
    );
    assert!(
        has_toast(&events, "empty response repeatedly"),
        "the chat turn's exhausted stop stays visible: {events:?}"
    );
    assert_eq!(
        h.pane_resumes_left(),
        Harness::pane_resume_budget(),
        "a loop that owes no report never touches the resume budget"
    );
}

/// The bound is what makes the continuation safe: a model that keeps
/// returning nothing terminates VISIBLY instead of renewing forever (the
/// inner nudges reset on every ENTRY, so only a per-loop budget bounds it).
#[tokio::test]
async fn a_pane_stops_once_its_resume_budget_is_spent() {
    let mut h = pane().with_mock_streams(vec![
        Ok(vec![]),
        Ok(vec![]),
        Ok(vec![]),
        Ok(vec!["never reached"]),
    ]);
    h.set_stop_nudges_left_for_test(0);

    let (h, events) = drive(h).await;

    assert_eq!(h.pane_resumes_left(), 0, "every resume must consume budget");
    assert_eq!(
        h.context_manager.final_answer(),
        None,
        "with the budget spent the stop stands: no Closure (reported as \
         `incomplete` downstream, never `cancelled`)"
    );
    assert!(
        has_toast(&events, "empty response repeatedly"),
        "the exhausted stop must stay visible: {events:?}"
    );
}

/// `resume_pane_turn` refuses when the USER stopped the turn — the one
/// legitimate early end, and therefore the one that must never be overridden.
#[tokio::test]
async fn the_user_stop_is_never_resumed() {
    let mut h = pane();
    let stopped = AtomicBool::new(true);
    assert!(
        h.resume_pane_turn(&stopped, ResumeCause::EmptyStop).is_none(),
        "a user stop must stand: resuming would override a deliberate interrupt"
    );
    assert_eq!(
        h.pane_resumes_left(),
        Harness::pane_resume_budget(),
        "a refusal must not consume the resume budget"
    );

    // With the signal CLEAR the resume is granted, and the reasoning of the
    // cut attempt rides the steering message: the model already paid for it,
    // and re-deriving it wastes a whole attempt of a turn already behind.
    h.set_attempt_reasoning_for_test("step 1: read the file; step 2: report back");
    let steering = h
        .resume_pane_turn(&AtomicBool::new(false), ResumeCause::EmptyStop)
        .expect("a pane that owes a report must resume");
    assert!(
        steering.contains("step 1: read the file; step 2: report back"),
        "the cut attempt's reasoning must be promoted: {steering}"
    );
    assert!(
        steering.contains("final report"),
        "the steering must name the deliverable: {steering}"
    );
}

/// A loop that owes NO report never resumes, so the main chat is untouched.
#[tokio::test]
async fn a_loop_that_owes_no_report_never_resumes() {
    let mut h = Harness::new_test();
    assert!(!h.owes_report(), "the main chat loop owes no report");
    assert!(
        h.resume_pane_turn(&AtomicBool::new(false), ResumeCause::EmptyStop)
            .is_none(),
        "the main loop must keep its final stop"
    );
    assert!(
        pane().owes_report(),
        "a bound pane owes its requester a report"
    );
    assert!(
        Harness::new_test().as_nested().owes_report(),
        "the internal sub-agent owes its caller a report"
    );
}

/// The repetition detector's stop is an improper stop for a pane too: the
/// model looks stuck, but nobody asked the turn to end — so it resumes, with
/// the streak reset so the resumed turn is judged on its own output.
#[tokio::test]
async fn repetition_detection_resumes_a_pane_instead_of_stopping() {
    let same = "I still have to write the report, so let me keep working on it a while longer.";
    assert!(same.len() >= 64, "the text must be repetition-detectable");
    let h = pane()
        .with_mock_streams(vec![
            Ok(vec![same]),
            Ok(vec![same]),
            Ok(vec![same]),
            Ok(vec!["the report"]),
        ])
        // `length` keeps the loop alive past the first two turns: a clean
        // `stop` would close the turn on turn 1 and never reach the streak.
        .with_mock_finish_reason(Some("length"))
        .with_mock_finish_reason(Some("length"));

    let (h, events) = drive(h).await;

    let items: Vec<ContextItem> = h.context_manager.items_snapshot();
    match items.last() {
        Some(ContextItem::Closure { content, .. }) => assert_eq!(content, "the report"),
        other => panic!("the pane must resume past the repetition stop, got {other:?}"),
    }
    assert!(
        !has_toast(&events, "repeated the same output"),
        "a pane must not be stopped by the repetition detector while it owes a \
         report: {events:?}"
    );
    assert_eq!(
        h.pane_resumes_left(),
        Harness::pane_resume_budget() - 1,
        "the repetition resume must consume exactly one unit of the bound"
    );
}
