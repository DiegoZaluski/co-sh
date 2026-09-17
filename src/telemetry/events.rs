//! Telemetry event envelopes and payloads.
//!
//! Every wire type is a closed struct with PRIVATE fields: the only way to
//! build one is a validated constructor, so no free-form string can cross the
//! privacy boundary by construction. Deserialized envelopes (from
//! the queue file) are re-validated through [`EventEnvelope::validate`] before
//! they can be sent (persisted data is untrusted).

use std::collections::BTreeMap;

use chrono::{Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::telemetry::schema::{
    ErrorCategory, ErrorSource, EventType, Feature, SCHEMA_MIN_SUPPORTED_VERSION, SCHEMA_VERSION,
};

/// Maximum number of distinct keys per aggregate map (cardinality cap:
/// accumulators must be bounded).
pub const MAX_CARDINALITY: usize = 32;

/// Maximum distinct error representations per session.
pub const MAX_ERRORS: usize = 64;

// Validated scalar components

/// Validated app version: `major.minor[.patch]`, digits only. An app version
/// is not an IP address — validated by shape, never by the denylist (audit
/// F11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppVersion(String);

impl AppVersion {
    pub fn validate(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > 32 {
            return None;
        }
        let parts: Vec<&str> = raw.split('.').collect();
        let ok = (2..=3).contains(&parts.len())
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_digit()));
        ok.then(|| Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build channel (closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stable,
    Nightly,
    Dev,
}

impl Channel {
    pub fn validate(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "nightly" => Some(Self::Nightly),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
            Self::Dev => "dev",
        }
    }
}

fn is_uuid_v4_shape(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
        && parts[2].starts_with('4')
        // RFC 9562 §4.1: variant must be 10xx — first char of group 4 in 8/9/a/b.
        && parts[3]
            .starts_with(['8', '9', 'a', 'b', 'A', 'B'])
}

/// Validated random identifier (install id, session id, client event id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UuidId(String);

impl UuidId {
    pub fn validate(raw: &str) -> Option<Self> {
        is_uuid_v4_shape(raw).then(|| Self(raw.to_string()))
    }

    /// Fresh id from OS entropy. `None` when entropy is unavailable: the
    /// caller drops the event (no time/address-derived fallback).
    pub fn generate() -> Option<Self> {
        Self::validate(&uuid_v4()?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// UTC timestamp rounded DOWN to the minute, RFC 3339 (coarsening, plan §1:
/// sub-minute precision is re-identification surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurredAt(String);

impl OccurredAt {
    pub fn now() -> Self {
        Self(Utc::now().format("%Y-%m-%dT%H:%M:00Z").to_string())
    }

    /// Accepts only an already-minute-rounded RFC 3339 timestamp.
    pub fn validate(raw: &str) -> Option<Self> {
        let t = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
        let utc = t.with_timezone(&Utc);
        (utc.second() == 0 && utc.nanosecond() == 0).then(|| Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Random UUIDv4 from OS entropy. Returns `None` when the OS entropy source
/// fails — the caller DROPS the telemetry event. There is deliberately no
/// time- or address-derived fallback.
pub fn uuid_v4() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10
    let h = |range: std::ops::Range<usize>| {
        bytes[range]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    Some(format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    ))
}

// Payloads

/// Compile-time OS name (never probed from the environment).
pub const OS_NAME: &str = if cfg!(target_os = "linux") {
    "linux"
} else if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "other"
};

/// Compile-time target arch.
pub const ARCH_NAME: &str = if cfg!(target_arch = "x86_64") {
    "x86_64"
} else if cfg!(target_arch = "aarch64") {
    "aarch64"
} else {
    "other"
};

/// Payload for `install` — one per installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPayload {
    os: String,
    arch: String,
    /// First-run date only ("YYYY-MM-DD"), never a timestamp.
    first_run_day: String,
}

impl InstallPayload {
    /// Build from compile-time target info plus a validated `YYYY-MM-DD`.
    /// Shape is checked strictly (zero-padded), then calendar validity via
    /// chrono ("2026-99-99" must be rejected).
    pub fn new(first_run_day: &str) -> Option<Self> {
        let day = first_run_day.trim();
        let bytes = day.as_bytes();
        let shape_ok = day.len() == 10
            && bytes.iter().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    *b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            });
        if !shape_ok {
            return None;
        }
        chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .ok()
            .map(|_| Self {
                os: OS_NAME.to_string(),
                arch: ARCH_NAME.to_string(),
                first_run_day: day.to_string(),
            })
    }

    pub fn os(&self) -> &str {
        &self.os
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    pub fn first_run_day(&self) -> &str {
        &self.first_run_day
    }

    pub fn validate(&self) -> bool {
        InstallPayload::new(&self.first_run_day).is_some()
            // os/arch come from cfg! at construction, but a tampered queue
            // line could carry anything — re-check them.
            && self.os == OS_NAME
            && self.arch == ARCH_NAME
    }
}

/// A single aggregated error representation — NEVER a raw message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRepr {
    category: ErrorCategory,
    /// xxh64 of the normalized message (`sanitize::fingerprint_error`).
    fingerprint: String,
    /// Validated internal module path (never free text).
    source: ErrorSource,
    /// Normalized provider name when the error is provider-related.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// Times this fingerprint occurred during the session.
    occurrence: u32,
}

impl ErrorRepr {
    pub fn new(
        category: ErrorCategory,
        fingerprint: &str,
        source: ErrorSource,
        provider: Option<String>,
        occurrence: u32,
    ) -> Option<Self> {
        let fp_ok = fingerprint.len() == 16 && fingerprint.chars().all(|c| c.is_ascii_hexdigit());
        fp_ok.then(|| Self {
            category,
            fingerprint: fingerprint.to_string(),
            source,
            provider,
            occurrence,
        })
    }

    pub fn category(&self) -> ErrorCategory {
        self.category
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn source(&self) -> &ErrorSource {
        &self.source
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    /// Invariant re-check used when re-validating deserialized envelopes.
    pub fn validate(&self) -> bool {
        self.fingerprint.len() == 16
            && self.fingerprint.chars().all(|c| c.is_ascii_hexdigit())
            && ErrorSource::validate(self.source.as_str()).is_some()
            && self
                .provider
                .as_deref()
                .map(crate::telemetry::sanitize::is_normalized_identifier)
                .unwrap_or(true)
            && self.occurrence > 0
    }
}

/// Payload for standalone `error` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    #[serde(flatten)]
    error: ErrorRepr,
}

impl ErrorPayload {
    pub fn new(
        category: ErrorCategory,
        fingerprint: &str,
        source: ErrorSource,
        provider: Option<String>,
    ) -> Option<Self> {
        Some(Self {
            error: ErrorRepr::new(category, fingerprint, source, provider, 1)?,
        })
    }

    pub fn error(&self) -> &ErrorRepr {
        &self.error
    }

    pub fn validate(&self) -> bool {
        self.error.validate()
    }
}

/// Payload for `session_summary` — the core aggregated event (1 per session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummaryPayload {
    /// Wall-clock usage seconds, rounded to multiples of 30.
    duration_sec: u64,
    turns: u32,
    messages: u32,
    /// provider (normalized/allowlisted) → request count.
    providers: BTreeMap<String, u32>,
    /// model (normalized or hashed) → request count.
    models: BTreeMap<String, u32>,
    /// feature enum name → usage count.
    features: BTreeMap<String, u32>,
    /// tool (allowlisted or "other") → call count.
    tools: BTreeMap<String, u32>,
    /// MCP server count only — average servers per user (identity deferred,
    /// plan §4.6).
    mcp_servers: u32,
    compactions: u32,
    tokens_in: u64,
    tokens_out: u64,
    /// Real provider-reported cost, rounded ONCE from the fixed-point sum
    ///.
    cost_usd_cents: u32,
    /// Error representations: category + fingerprint + occurrences.
    errors: Vec<ErrorRepr>,
    update_banner_shown: bool,
}

impl SessionSummaryPayload {
    /// Assemble from session aggregates. `cost_usd_cents` must be the
    /// single-rounding result of the fixed-point accumulator.
    #[allow(clippy::too_many_arguments)]
    pub fn from_aggregates(
        duration_sec: u64,
        turns: u32,
        messages: u32,
        providers: BTreeMap<String, u32>,
        models: BTreeMap<String, u32>,
        features: BTreeMap<String, u32>,
        tools: BTreeMap<String, u32>,
        mcp_servers: u32,
        compactions: u32,
        tokens_in: u64,
        tokens_out: u64,
        cost_usd_cents: u32,
        errors: Vec<ErrorRepr>,
        update_banner_shown: bool,
    ) -> Self {
        Self {
            duration_sec,
            turns,
            messages,
            providers,
            models,
            features,
            tools,
            mcp_servers,
            compactions,
            tokens_in,
            tokens_out,
            cost_usd_cents,
            errors,
            update_banner_shown,
        }
    }

    pub fn duration_sec(&self) -> u64 {
        self.duration_sec
    }

    pub fn turns(&self) -> u32 {
        self.turns
    }

    pub fn messages(&self) -> u32 {
        self.messages
    }

    pub fn providers(&self) -> &BTreeMap<String, u32> {
        &self.providers
    }

    pub fn models(&self) -> &BTreeMap<String, u32> {
        &self.models
    }

    pub fn features(&self) -> &BTreeMap<String, u32> {
        &self.features
    }

    pub fn tools(&self) -> &BTreeMap<String, u32> {
        &self.tools
    }

    pub fn mcp_servers(&self) -> u32 {
        self.mcp_servers
    }

    pub fn compactions(&self) -> u32 {
        self.compactions
    }

    pub fn tokens_in(&self) -> u64 {
        self.tokens_in
    }

    pub fn tokens_out(&self) -> u64 {
        self.tokens_out
    }

    pub fn cost_usd_cents(&self) -> u32 {
        self.cost_usd_cents
    }

    pub fn errors(&self) -> &[ErrorRepr] {
        &self.errors
    }

    pub fn update_banner_shown(&self) -> bool {
        self.update_banner_shown
    }

    pub fn validate(&self) -> bool {
        let maps_ok = [&self.providers, &self.models, &self.features, &self.tools]
            .iter()
            .all(|m| {
                m.len() <= MAX_CARDINALITY
                    // Key CONTENT matters: a tampered queue line could carry
                    // a project codename in place of a normalized identifier
                    //. Allowlisted names and 16-hex hashes pass.
                    && m.keys().all(|k| {
                        k.len() <= 64 && crate::telemetry::sanitize::is_normalized_identifier(k)
                    })
            });
        maps_ok && self.errors.len() <= MAX_ERRORS && self.errors.iter().all(ErrorRepr::validate)
    }
}

/// Payload for `crash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashPayload {
    category: ErrorCategory,
    /// Validated internal module path of the panic location.
    source: ErrorSource,
    app_uptime_sec: u64,
}

impl CrashPayload {
    pub fn new(category: ErrorCategory, source: ErrorSource, app_uptime_sec: u64) -> Self {
        Self {
            category,
            source,
            app_uptime_sec,
        }
    }

    pub fn category(&self) -> ErrorCategory {
        self.category
    }

    pub fn source(&self) -> &ErrorSource {
        &self.source
    }

    pub fn app_uptime_sec(&self) -> u64 {
        self.app_uptime_sec
    }

    pub fn validate(&self) -> bool {
        ErrorSource::validate(self.source.as_str()).is_some()
    }
}

/// Payload for `update` — version adoption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePayload {
    from: AppVersion,
    to: AppVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<Channel>,
}

impl UpdatePayload {
    pub fn new(from: AppVersion, to: AppVersion, channel: Option<Channel>) -> Self {
        Self { from, to, channel }
    }

    pub fn validate(&self) -> bool {
        // Re-checked on purpose: AppVersion's derived Deserialize does NOT
        // run the constructor validation, so a tampered queue line could
        // carry free text in from/to.
        AppVersion::validate(self.from.as_str()).is_some()
            && AppVersion::validate(self.to.as_str()).is_some()
    }
}

/// Closed payload set — the variant MUST match the envelope's event type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventPayload {
    Install(InstallPayload),
    SessionSummary(SessionSummaryPayload),
    Error(ErrorPayload),
    Crash(CrashPayload),
    Update(UpdatePayload),
}

impl EventPayload {
    /// Does this payload variant correspond to `event_type`?
    pub fn matches(&self, event_type: EventType) -> bool {
        matches!(
            (event_type, self),
            (EventType::Install, EventPayload::Install(_))
                | (EventType::SessionSummary, EventPayload::SessionSummary(_))
                | (EventType::Error, EventPayload::Error(_))
                | (EventType::Crash, EventPayload::Crash(_))
                | (EventType::Update, EventPayload::Update(_))
        )
    }

    /// Per-payload invariant re-check (validated again after
    /// deserialization, before sending).
    pub fn validate(&self) -> bool {
        match self {
            EventPayload::Install(p) => p.validate(),
            EventPayload::SessionSummary(p) => p.validate(),
            EventPayload::Error(p) => p.validate(),
            EventPayload::Crash(p) => p.validate(),
            EventPayload::Update(p) => p.validate(),
        }
    }
}

// Envelope

/// Envelope shared by every event — maps 1:1 to the `telemetry_events` row.
/// All fields are private: build through [`EventEnvelope::new`] only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Schema version of THIS event (per-event, not global).
    schema_version: u16,
    app_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    /// Random local UUIDv4 (install cohort), no relation to any identity.
    install_id: String,
    /// Usage session id (absent for install events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    event_type: EventType,
    /// Client-generated dedupe/idempotency id.
    client_event_id: String,
    occurred_at: String,
    /// Payload — the variant always matches `event_type`.
    payload: EventPayload,
}

impl EventEnvelope {
    /// Build a fully validated envelope. Returns `None` (event dropped) when
    /// any component fails validation or OS entropy is unavailable.
    pub fn new(
        event_type: EventType,
        app_version: &AppVersion,
        channel: Option<Channel>,
        install_id: &UuidId,
        session_id: Option<&UuidId>,
        occurred_at: OccurredAt,
        payload: EventPayload,
    ) -> Option<Self> {
        if !payload.matches(event_type) || !payload.validate() {
            return None;
        }
        Some(Self {
            schema_version: SCHEMA_VERSION,
            app_version: app_version.as_str().to_string(),
            channel: channel.map(|c| c.as_str().to_string()),
            install_id: install_id.as_str().to_string(),
            session_id: session_id.map(|s| s.as_str().to_string()),
            event_type,
            client_event_id: UuidId::generate()?.as_str().to_string(),
            occurred_at: occurred_at.as_str().to_string(),
            payload,
        })
    }

    /// Re-validate a (possibly deserialized) envelope. The sink runs this on
    /// every persisted event before sending — queue content is untrusted
    ///.
    pub fn validate(&self) -> bool {
        let scalars_ok = (SCHEMA_MIN_SUPPORTED_VERSION..=SCHEMA_VERSION)
            .contains(&self.schema_version)
            && AppVersion::validate(&self.app_version).is_some()
            && self
                .channel
                .as_deref()
                .map(|c| Channel::validate(c).is_some())
                .unwrap_or(true)
            && UuidId::validate(&self.install_id).is_some()
            && self
                .session_id
                .as_deref()
                .map(|s| UuidId::validate(s).is_some())
                .unwrap_or(true)
            && UuidId::validate(&self.client_event_id).is_some()
            && OccurredAt::validate(&self.occurred_at).is_some()
            && self.payload.matches(self.event_type)
            && self.payload.validate();
        // Belt-and-braces denylist over the PAYLOAD only. Scalar metadata
        // (app_version, ids, timestamp) is validated by typed components
        // above — running the denylist over the whole envelope would
        // regression guard: "10." must keep being accepted in app_version 0.10.0.
        //
        // Update payloads are exempt for the same reason:
        // their from/to fields are shape-validated AppVersion values, and
        // the substring scan false-positives on the "10." fragment inside
        // a legitimate 0.10.0 version.
        scalars_ok
            && (matches!(self.payload, EventPayload::Update(_))
                || crate::telemetry::sanitize::validate_serialized(
                    &serde_json::to_string(&self.payload).unwrap_or_default(),
                ))
    }

    pub fn event_type(&self) -> EventType {
        self.event_type
    }

    /// Stable wire name of the wrapped event type.
    pub fn type_name(&self) -> &'static str {
        self.event_type.as_str()
    }

    pub fn client_event_id(&self) -> &str {
        &self.client_event_id
    }

    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    pub fn occurred_at(&self) -> &str {
        &self.occurred_at
    }

    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Test-only: replace the occurred-at stamp with another VALID, already
    /// minute-rounded timestamp (used to build expired queue fixtures).
    #[doc(hidden)]
    pub fn with_occurred_at(mut self, ts: &str) -> Option<Self> {
        self.occurred_at = OccurredAt::validate(ts)?.as_str().to_string();
        Some(self)
    }

    /// Test-only: inflate the serialized size by swapping the payload's error
    /// source for an over-long filler (exercises the queue size cap). The
    /// result intentionally fails `validate` — `push` checks size first.
    #[doc(hidden)]
    pub fn with_padding(mut self, bytes: usize) -> Self {
        if let EventPayload::Error(p) = &mut self.payload {
            p.error.source = ErrorSource::test_filler(bytes);
        }
        self
    }
}

// Counters

/// Saturating counter bump (counters never panic or wrap).
///
/// NOTE: this does NOT enforce [`MAX_CARDINALITY`] — new keys always enter
/// the map. Prefer [`bump_capped`] for aggregate maps that later flow into
/// [`SessionSummaryPayload`]; an over-cardinality map fails validation at
/// enqueue/send time and the WHOLE event is dropped.
pub fn bump(map: &mut BTreeMap<String, u32>, key: &str) {
    let entry = map.entry(key.to_string()).or_insert(0);
    *entry = entry.saturating_add(1);
}

/// Cardinality-capped bump: keys beyond [`MAX_CARDINALITY`] are dropped
/// (fail closed) so an accumulator can never grow unbounded.
pub fn bump_capped(map: &mut BTreeMap<String, u32>, key: &str) {
    if !map.contains_key(key) && map.len() >= MAX_CARDINALITY {
        return;
    }
    bump(map, key);
}

/// Feature counter helper taking the enum directly.
pub fn bump_feature(map: &mut BTreeMap<String, u32>, feature: Feature) {
    bump_capped(map, feature.as_str());
}

/// Build a fully VALID envelope with synthetic, safe content (zeroed
/// aggregates, placeholder ids). `#[doc(hidden)]`: test and documentation
/// helper only — production code builds envelopes from real, validated
/// aggregates through [`EventEnvelope::new`].
#[doc(hidden)]
pub fn synthetic(event_type: EventType) -> EventEnvelope {
    let source = ErrorSource::validate("harness::core").expect("valid synthetic source");
    let payload = match event_type {
        EventType::Install => {
            EventPayload::Install(InstallPayload::new("2025-06-01").expect("valid day"))
        }
        EventType::SessionSummary => {
            EventPayload::SessionSummary(SessionSummaryPayload::from_aggregates(
                0,
                0,
                0,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                0,
                0,
                0,
                0,
                0,
                Vec::new(),
                false,
            ))
        }
        EventType::Error => EventPayload::Error(
            ErrorPayload::new(ErrorCategory::Panic, "0123456789abcdef", source, None)
                .expect("valid synthetic error"),
        ),
        EventType::Crash => EventPayload::Crash(CrashPayload::new(ErrorCategory::Panic, source, 0)),
        EventType::Update => EventPayload::Update(UpdatePayload::new(
            AppVersion::validate("0.1.0").expect("valid version"),
            AppVersion::validate("0.2.0").expect("valid version"),
            None,
        )),
    };
    EventEnvelope::new(
        event_type,
        &AppVersion::validate("0.1.0").expect("valid version"),
        None,
        &UuidId::generate().expect("OS entropy"),
        None,
        OccurredAt::now(),
        payload,
    )
    .expect("valid synthetic envelope")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_version() -> AppVersion {
        AppVersion::validate("0.1.0").unwrap()
    }

    #[test]
    fn uuid_v4_shape_and_uniqueness() {
        let a = uuid_v4().expect("OS entropy available");
        let b = uuid_v4().expect("OS entropy available");
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert!(a.as_bytes()[14] == b'4'); // version nibble
        for part in a.split('-') {
            assert!(part.bytes().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn app_version_validation_accepts_semver_rejects_free_text() {
        assert!(AppVersion::validate("0.10.0").is_some());
        assert!(AppVersion::validate("1.2").is_some());
        assert!(AppVersion::validate("not-a-uuid").is_none());
        assert!(AppVersion::validate("1.2.3-alpha").is_none());
        assert!(AppVersion::validate("").is_none());
    }

    #[test]
    fn envelope_construction_rejects_variant_type_mismatch() {
        let payload = EventPayload::Install(InstallPayload::new("2025-06-01").unwrap());
        assert!(
            EventEnvelope::new(
                EventType::Crash,
                &app_version(),
                None,
                &UuidId::generate().unwrap(),
                None,
                OccurredAt::now(),
                payload,
            )
            .is_none()
        );
    }

    #[test]
    fn tampered_envelope_fails_validation() {
        let payload = EventPayload::Install(InstallPayload::new("2025-06-01").unwrap());
        let env = EventEnvelope::new(
            EventType::Install,
            &app_version(),
            None,
            &UuidId::generate().unwrap(),
            None,
            OccurredAt::now(),
            payload,
        )
        .unwrap();
        assert!(env.validate());
        let mut value = serde_json::to_value(&env).unwrap();
        value["install_id"] = serde_json::json!("not-a-uuid");
        value["schema_version"] = serde_json::json!(u16::MAX);
        value["occurred_at"] = serde_json::json!("2026-09-16T12:34:56.123456Z");
        let tampered: EventEnvelope = serde_json::from_value(value).unwrap();
        assert!(!tampered.validate());
    }

    #[test]
    fn error_repr_serializes_category_as_snake_case() {
        let repr = ErrorRepr::new(
            ErrorCategory::ProviderRateLimit,
            "abcdef0123456789",
            ErrorSource::validate("harness::core").unwrap(),
            Some("openai".into()),
            2,
        )
        .unwrap();
        let json = serde_json::to_value(&repr).expect("serialize");
        assert_eq!(json["category"], "provider_rate_limit");
        assert!(repr.validate());
    }

    #[test]
    fn error_repr_rejects_bad_fingerprints() {
        assert!(
            ErrorRepr::new(
                ErrorCategory::Panic,
                "short",
                ErrorSource::validate("harness::core").unwrap(),
                None,
                1,
            )
            .is_none()
        );
    }

    #[test]
    fn bump_is_saturating() {
        let mut map = BTreeMap::from([("bash".to_string(), u32::MAX)]);
        bump(&mut map, "bash");
        assert_eq!(map["bash"], u32::MAX); // no panic, no wrap
    }

    #[test]
    fn bump_capped_enforces_cardinality() {
        let mut map = BTreeMap::new();
        for i in 0..(MAX_CARDINALITY + 10) {
            bump_capped(&mut map, &format!("key{i}"));
        }
        assert_eq!(map.len(), MAX_CARDINALITY);
    }
}
