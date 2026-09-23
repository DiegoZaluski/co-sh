//! Cost-tracking tests: the provider-reported REAL cost is the ONLY source
//! of truth. Requests whose provider does not report a cost stay unpriced —
//! no models.dev estimate is applied anymore (providers without cost
//! reporting simply show no price; see `cosh_sdk::connector::supports_cost_reporting`).

use super::{App, HOME_LOCK, isolate_home};

#[test]
fn reported_cost_is_the_source_of_truth() {
    // The provider reported $0.42 — recorded verbatim, never re-priced.
    let cost = App::resolve_recorded_cost(Some(0.42));
    assert_eq!(cost, Some(0.42));
}

#[test]
fn no_reported_cost_stays_unpriced() {
    // No estimate fallback: the record stays unpriced and is excluded
    // from dollar totals instead of showing a guessed figure.
    let cost = App::resolve_recorded_cost(None);
    assert_eq!(cost, None);
}

#[test]
fn zero_reported_cost_is_real_and_not_re_priced() {
    // Free-tier models genuinely report $0 — kept as a real (zero) cost.
    let cost = App::resolve_recorded_cost(Some(0.0));
    assert_eq!(cost, Some(0.0));
}

/// Regression for the wrong-session cost bug: a Usage event lands while the
/// user is VIEWING a different session than the one OWNING the running agent
/// loop. The record (its cost AND tokens) must go to the owner's session —
/// the viewed session's dashboard must not absorb it, and the owner's must.
#[tokio::test]
async fn usage_event_attributed_to_loop_owner_not_viewed_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());

    // "a" owns the running loop; "b" is merely being viewed when the event
    // arrives (async arrival after a session switch).
    app.state.add_empty_session("a".into(), "a".into(), 0);
    app.state.add_empty_session("b".into(), "b".into(), 0);
    app.state.current_session_id = Some("b".into());
    app.active_loop_session_id = Some("a".into());

    let usage = cosh_sdk::connector::TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        ..Default::default()
    };
    app.record_usage(usage, "charm", "glm-5.3-flash", Some(0.000012), None);

    assert_eq!(app.usage_records.len(), 1);
    assert_eq!(app.usage_records[0].session_id, "a");

    // The dashboard snapshot for the VIEWED session stays clean…
    let viewed = app.session_records();
    assert!(viewed.is_empty(), "viewed session must not absorb the cost");

    // …and the OWNER's session sees the real figure.
    app.state.current_session_id = Some("a".into());
    let owned = app.session_records();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].cost_usd, Some(0.000012));
    assert_eq!(crate::usage::total_tokens(&owned), 150);
}

/// Without a loop owner (idle usage events, e.g. a background compaction
/// surfaced late), the currently-viewed session remains the fallback —
/// same resolution order as every other async event handler.
#[tokio::test]
async fn usage_event_falls_back_to_current_session_without_loop_owner() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("s".into(), "s".into(), 0);
    app.state.current_session_id = Some("s".into());
    app.active_loop_session_id = None;

    app.record_usage(
        cosh_sdk::connector::TokenUsage::default(),
        "charm",
        "m",
        Some(0.01),
        None,
    );
    assert_eq!(app.usage_records[0].session_id, "s");
}

/// Regression for the vanishing-cost case: usage arrives while the user went
/// back Home (`current_session_id == None`) with the loop still running.
/// The old code recorded `session_id: ""`, which showed in NO session block;
/// the record must land on the loop owner instead.
#[tokio::test]
async fn usage_event_at_home_lands_on_loop_owner_not_empty() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("a".into(), "a".into(), 0);
    app.active_loop_session_id = Some("a".into());
    // Back on Home: no current session, but the loop still runs in background.
    app.state.current_session_id = None;

    app.record_usage(
        cosh_sdk::connector::TokenUsage::default(),
        "charm",
        "m",
        Some(0.5),
        None,
    );
    assert_eq!(app.usage_records.len(), 1);
    assert_eq!(
        app.usage_records[0].session_id, "a",
        "cost must not vanish into an empty session id"
    );
}
