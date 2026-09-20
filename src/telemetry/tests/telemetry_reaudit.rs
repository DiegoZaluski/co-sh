//! Re-audit (TELEMETRY_AUDIT.md, C01–C05) regression tests.
//!
//! Every case below asserts the SAFE outcome required of the remediation —
//! the original `repro_*` tests asserted the DEFECTIVE behavior and were
//! converted after the fixes, exactly like the F-series in
//! `telemetry_audit.rs`.
//!
//! The transport-level cases (C02 redirect/consent override, C03 HTTP-date
//! over the wire, 5xx re-queue) live in `src/telemetry/transport_tests.rs`
//! INSIDE the crate: since re-audit C02 the sender, the hardened client and
//! the consent token are `pub(crate)`, so external test code cannot (and
//! must not) reach them. Only the payload-validation surface (C01, C04,
//! C05) remains publicly testable.
//!
//! Only synthetic data and temporary directories are used.

use crate::telemetry::events::{
    AppVersion, EventEnvelope, EventPayload, InstallPayload, OccurredAt, UpdatePayload, UuidId,
};
use crate::telemetry::schema::{ErrorCategory, ErrorSource, EventType};
use crate::telemetry::session::SessionTelemetry;

fn wrap(kind: EventType, payload: EventPayload) -> EventEnvelope {
    EventEnvelope::new(
        kind,
        &AppVersion::validate("0.1.0").unwrap(),
        None,
        &UuidId::generate().unwrap(),
        None,
        OccurredAt::now(),
        payload,
    )
    .unwrap()
}

#[test]
fn c01_private_project_identifiers_cannot_enter_error_source() {
    // Was repro_source_charset_still_accepts_private_project_content.
    // ErrorSource::validate is now a CLOSED allowlist: a private-looking but
    // syntactically valid module path must be REJECTED at the boundary.
    for source in [
        "acme::confidential_merger",
        "customer::internal_project",
        "alice::secret_stuff",
    ] {
        assert!(
            ErrorSource::validate(source).is_none(),
            "non-allowlisted source must be rejected: {source}"
        );
    }
    // Positive control: the real, reviewed module names still pass.
    assert!(ErrorSource::validate("harness::core").is_some());

    // And the session accumulator drops the event instead of serializing it.
    let mut session = SessionTelemetry::new();
    session.record_error(
        ErrorCategory::FsIo,
        "acme::confidential_merger",
        None,
        "failed",
    );
    let envelope = wrap(
        EventType::SessionSummary,
        EventPayload::SessionSummary(session.finish()),
    );
    let json = serde_json::to_string(&envelope).unwrap();
    assert!(
        !json.contains("confidential_merger"),
        "the private identifier must never reach the payload"
    );
}

#[test]
fn c04_update_versions_with_the_10_fragment_validate() {
    // Was repro_update_version_still_matches_private_ip_denylist: a fully
    // valid Update event carrying the version 0.10.0 failed the envelope's
    // final payload guard (the "10." fragment matches the private-IP
    // pattern). Update payloads are now exempt from the substring scan —
    // their from/to fields are shape-validated typed values instead.
    let update = UpdatePayload::new(
        AppVersion::validate("0.9.0").unwrap(),
        AppVersion::validate("0.10.0").unwrap(),
        None,
    );
    assert!(update.validate(), "0.9.0 -> 0.10.0 is a legitimate update");
    let envelope = wrap(EventType::Update, EventPayload::Update(update));
    assert!(
        envelope.validate(),
        "the denylist false-positive on 0.10.0 must be gone"
    );
}

#[test]
fn c05_impossible_calendar_dates_and_wrong_uuid_variants_are_rejected() {
    // Was repro_invalid_calendar_date_and_uuid_variant_are_accepted.
    // (1) The install date is calendar-validated, not just shape-checked.
    assert!(
        InstallPayload::new("2026-99-99").is_none(),
        "month 99 must fail"
    );
    assert!(
        InstallPayload::new("2026-02-30").is_none(),
        "Feb 30 must fail"
    );
    let envelope = wrap(
        EventType::Install,
        EventPayload::Install(InstallPayload::new("2025-06-01").expect("a real date")),
    );
    assert!(envelope.validate(), "a real calendar date still passes");

    // (2) UUID group 4 must start with the RFC 9562 variant bits 10xx
    // (8/9/a/b); a `0` first nibble is not a v4 variant.
    assert!(
        UuidId::validate("00000000-0000-4000-0000-000000000000").is_none(),
        "wrong variant nibble must be rejected"
    );
    assert!(UuidId::validate("00000000-0000-4000-8000-000000000000").is_some());
}
