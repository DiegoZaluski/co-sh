//! Regression tests for the external telemetry audit (TELEMETRY_AUDIT.md).
//!
//! The original `repro_*` tests asserted the DEFECTIVE behavior (they passed
//! because they reproduced the findings). After remediation, each now asserts
//! the required SAFE outcome instead (audit "Remediation order and acceptance
//! criteria", lines 266–271).
//!
//! Deferred findings (F05, F06, F09, F10, F15) require P2 architectural work
//! and are intentionally NOT covered here — see
//! `src/telemetry/.NOTE/DEFERRED-P2.md`. F03's Retry-After wait is verified
//! at unit level (`telemetry::sink::tests::retry_after_is_parsed_and_capped`):
//! exercising it end-to-end would require a TLS loopback server.
//!
//! Only synthetic data and temporary directories are used.

use crate::telemetry::events::{
    AppVersion, EventEnvelope, EventPayload, InstallPayload, OccurredAt, UuidId, bump, synthetic,
    uuid_v4,
};
use crate::telemetry::queue::EventQueue;
use crate::telemetry::sanitize::sanitize_string;
use crate::telemetry::schema::{ErrorCategory, EventType};
use crate::telemetry::session::SessionTelemetry;
use crate::telemetry::sink::{FlushOutcome, SinkConfig};
use std::collections::BTreeMap;

#[test]
fn f01_denylist_matches_lowercase_inputs() {
    // Was repro_mixed_case_denylist_entries_do_not_match: macOS paths and
    // JWT-shaped strings passed the guard because the patterns were mixed
    // case while input is lowercased.
    for value in [
        "/Users/alice/private.txt",
        "eyJhbGciOiJIUzI1NiJ9.synthetic.signature",
    ] {
        assert!(
            sanitize_string(value).is_none(),
            "denylist must reject {value}"
        );
    }
    // Positive control: plain content still passes.
    assert!(sanitize_string("plain text ok").is_some());
}

#[test]
fn f01_typed_payloads_cannot_carry_private_content() {
    // Was repro_all_event_types_accept_unstructured_private_content: free-form
    // payloads crossed the boundary. Now payloads are closed structs with
    // private fields and validated components — free text cannot be stored.
    for kind in [
        EventType::Install,
        EventType::SessionSummary,
        EventType::Error,
        EventType::Crash,
        EventType::Update,
    ] {
        let e = synthetic(kind);
        // Positive control: a correctly built envelope is valid.
        assert!(e.validate(), "{kind:?} synthetic envelope must validate");
    }

    // An error record with a free-text (email) source is dropped fail-closed.
    let mut s = SessionTelemetry::new();
    s.record_error(ErrorCategory::FsIo, "alice@example.test", None, "failed");
    assert!(
        s.finish().errors().is_empty(),
        "free-text source must be dropped"
    );

    // Tampering with persisted content fails re-validation (F02 belt):
    // a private string in `source` or `first_run_day` invalidates the event.
    let mut value = serde_json::to_value(synthetic(EventType::Error)).unwrap();
    value["payload"]["source"] = serde_json::json!("alice@example.test");
    let tampered: EventEnvelope = serde_json::from_value(value).unwrap();
    assert!(!tampered.validate());

    let mut value = serde_json::to_value(synthetic(EventType::Install)).unwrap();
    value["payload"]["first_run_day"] = serde_json::json!("my secret note");
    let tampered: EventEnvelope = serde_json::from_value(value).unwrap();
    assert!(!tampered.validate());
}

#[test]
fn f01_schema_ids_and_timestamps_are_validated() {
    // Was repro_schema_and_timestamp_are_not_validated_or_coarsened.
    let mut value = serde_json::to_value(synthetic(EventType::Install)).unwrap();
    value["install_id"] = serde_json::json!("not-a-uuid");
    value["schema_version"] = serde_json::json!(u16::MAX);
    value["occurred_at"] = serde_json::json!("2026-09-16T12:34:56.123456Z");
    let tampered: EventEnvelope = serde_json::from_value(value).unwrap();
    assert!(
        !tampered.validate(),
        "invalid ids/schema/timestamp must fail validation"
    );

    // Timestamps are coarsened to the minute at construction.
    let now = OccurredAt::now();
    assert!(
        now.as_str().ends_with(":00Z"),
        "timestamp must be minute-rounded: {}",
        now.as_str()
    );
}

#[test]
fn f11_valid_app_version_is_accepted() {
    // Was repro_valid_app_version_is_rejected_as_private_ip: the "10.",
    // substring in the denylist rejected app_version 0.10.0. Versions are
    // now validated by shape (AppVersion), never by the denylist.
    let version = AppVersion::validate("0.10.0").expect("0.10.0 is a valid app version");
    let install_id = UuidId::generate().expect("OS entropy available");
    let e = EventEnvelope::new(
        EventType::Install,
        &version,
        None,
        &install_id,
        None,
        OccurredAt::now(),
        EventPayload::Install(InstallPayload::new("2025-06-01").expect("valid day")),
    )
    .expect("a valid envelope with a 0.10.x version must build");
    assert!(e.validate());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn f02_flush_is_consent_gated() {
    // Was repro_flush_sends_disabled_and_unsanitized_queue: a disabled
    // facade's queue could be flushed anyway. Now the facade flush re-checks
    // consent at call time and never transmits when disabled.
    const NAME: &str = "f02_flush_is_consent_gated";
    if std::env::var("COSH_AUDIT_CHILD").as_deref() != Ok(NAME) {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env("COSH_AUDIT_CHILD", NAME)
            .env("COSH_TELEMETRY", "off")
            .env("CI", "1")
            .env("XDG_DATA_HOME", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let t = crate::telemetry::Telemetry::resolve(false);
    assert!(!t.enabled());
    assert!(
        t.queue()
            .path()
            .starts_with(std::env::var("XDG_DATA_HOME").unwrap())
    );
    // A preexisting queued event (even one that WOULD pass validation) is
    // never transmitted while telemetry is disabled: the queue stays intact.
    t.queue().push(&synthetic(EventType::Error));
    let config = SinkConfig {
        endpoint: "https://ref.supabase.co/functions/v1/telemetry-ingest".into(),
        publishable_key: "sb_publishable_synthetic_key".into(),
        max_attempts: 1,
    };
    assert_eq!(t.flush(&config).await, FlushOutcome::Idle);
    assert_eq!(
        t.queue().pending_count(),
        1,
        "disabled flush must not consume the queue"
    );
}

// f03 (server failures are transient and re-queued) moved to
// src/telemetry/transport_tests.rs
// (`control_actual_5xx_responses_preserve_events`,
// `control_503_retry_sends_identical_event_then_succeeds`): live-HTTP
// regressions test INSIDE the crate since the sender became
// crate-internal (re-audit C02).

#[test]
fn f08_endpoint_validation_pins_https() {
    // Was repro_http_and_cross_origin_redirect_forward_event. The
    // transport-level cases (https_only client, redirect refusal) live in
    // src/telemetry/transport_tests.rs since the sender became
    // crate-internal (re-audit C02). Here: the public config gate.
    let config = SinkConfig {
        endpoint: "http://127.0.0.1:1/ingest".into(),
        publishable_key: "sb_publishable_synthetic_key".into(),
        max_attempts: 1,
    };
    assert!(!config.validate(), "http endpoints must be rejected");
}

#[test]
fn f07_single_oversized_event_is_rejected() {
    // Was repro_single_oversized_event_exceeds_queue_cap: one 600 KiB event
    // was appended anyway. Oversized events are now rejected at push time.
    let dir = tempfile::tempdir().unwrap();
    let queue = EventQueue::new(dir.path());
    queue.push(&synthetic(EventType::Error).with_padding(600 * 1024));
    assert!(
        std::fs::metadata(queue.path())
            .map(|m| m.len())
            .unwrap_or(0)
            <= 512 * 1024,
        "the queue file must stay within its cap"
    );
    assert_eq!(queue.pending_count(), 0);
    assert!(queue.drain_batch().is_empty());
}

#[test]
fn f04_uuid_never_panics_and_failure_drops_the_event() {
    // Was repro_uuid_panics_when_entropy_file_cannot_be_opened: the old
    // fallback copied a 16-byte u128 into an 8-byte slice and panicked.
    // uuid_v4 now returns None on entropy failure (the CALLER drops the
    // event); there is no time/address-derived fallback by construction.
    let result = std::panic::catch_unwind(uuid_v4);
    let id = result.expect("uuid_v4 must never panic");
    // With OS entropy available the id is a valid UUIDv4...
    let id = id.expect("entropy is available in CI/test environments");
    assert!(UuidId::validate(&id).is_some());
    // ...and generate()/validate() agree on the shape.
    assert!(UuidId::generate().is_some());
}

#[test]
fn f12_cost_is_rounded_once_per_session() {
    // Was repro_per_request_cost_rounding_loses_session_cost: 100 × USD 0.004
    // (= USD 0.40) rounded away to zero. Cost now accumulates in fixed-point
    // micro-USD and is rounded to cents once, in finish().
    let mut session = SessionTelemetry::new();
    for _ in 0..100 {
        session.record_usage(1, 1, Some(0.004));
    }
    assert_eq!(session.finish().cost_usd_cents(), 40);
}

#[test]
fn f13_errors_group_by_full_dimension_set() {
    // Was repro_error_aggregation_discards_category_provider_and_source:
    // distinct failures were attributed to the first record. The grouping
    // key is now (category, provider, source, fingerprint).
    let mut session = SessionTelemetry::new();
    session.record_error(
        ErrorCategory::ProviderAuth,
        "harness::a",
        Some("openai"),
        "request failed",
    );
    session.record_error(
        ErrorCategory::ProviderNetwork,
        "harness::b",
        Some("claude"),
        "request failed",
    );
    let payload = session.finish();
    assert_eq!(
        payload.errors().len(),
        2,
        "distinct dimensions must stay distinct"
    );
    assert_eq!(payload.errors()[0].category(), ErrorCategory::ProviderAuth);
    assert_eq!(payload.errors()[0].provider(), Some("openai"));
    assert_eq!(payload.errors()[0].source().as_str(), "harness::a");
    assert_eq!(payload.errors()[0].occurrence(), 1);
    assert_eq!(
        payload.errors()[1].category(),
        ErrorCategory::ProviderNetwork
    );
    assert_eq!(payload.errors()[1].occurrence(), 1);
}

#[test]
fn f14_counters_are_saturating() {
    // Was repro_counter_is_not_saturating: bumping at u32::MAX panicked in
    // debug builds. Counters now saturate — no panic, no wrap.
    let mut map = BTreeMap::from([("bash".to_string(), u32::MAX)]);
    bump(&mut map, "bash");
    assert_eq!(map["bash"], u32::MAX);
}
