//! Per-session telemetry accumulator.
//!
//! Aggregates usage counters in memory during a TUI session and produces one
//! `session_summary` payload at exit (plan §4.2). Everything recorded here is
//! non-sensitive by construction: counters, allowlisted names and hashed ids
//! only. Recording is cheap (map increments) and must never block the UI.
//!
//! Invariants (from the external audit):
//! - cost is accumulated in fixed-point micro-USD and rounded to cents ONCE
//!   in [`SessionTelemetry::finish`];
//! - errors are grouped by the FULL dimension set (category, provider,
//!   source, fingerprint), never by fingerprint alone;
//! - every counter is saturating — telemetry never panics or wraps;
//! - error `source` must validate as an internal module path; anything else
//!   is dropped (fail closed).

use std::collections::BTreeMap;
use std::time::Instant;

use crate::telemetry::events::{
    ErrorRepr, MAX_ERRORS, SessionSummaryPayload, bump_capped, bump_feature,
};
use crate::telemetry::sanitize::{
    fingerprint_error, normalize_provider, normalize_tool, round_duration,
};
use crate::telemetry::schema::{ErrorCategory, ErrorSource, Feature, SCHEMA_VERSION};

/// Full grouping key for an error representation: identical
/// fingerprints with different category/provider/source stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ErrorKey {
    category: ErrorCategory,
    /// Normalized provider (empty when the error is not provider-related).
    provider: String,
    source: ErrorSource,
    fingerprint: String,
}

#[derive(Debug, Clone)]
struct ErrorSlot {
    occurrence: u32,
}

/// Session accumulator. Create one per TUI session; call the `record_*`
/// methods from instrumentation points; call [`SessionTelemetry::finish`] on
/// exit to build the `session_summary` payload.
#[derive(Debug)]
pub struct SessionTelemetry {
    started_at: Instant,
    turns: u32,
    messages: u32,
    providers: BTreeMap<String, u32>,
    models: BTreeMap<String, u32>,
    features: BTreeMap<String, u32>,
    tools: BTreeMap<String, u32>,
    errors: BTreeMap<ErrorKey, ErrorSlot>,
    mcp_servers: u32,
    compactions: u32,
    tokens_in: u64,
    tokens_out: u64,
    /// Accumulated cost in micro-USD (1e-6); rounded to cents once at exit.
    cost_usd_micros: u64,
    update_banner_shown: bool,
}

impl Default for SessionTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTelemetry {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            turns: 0,
            messages: 0,
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            features: BTreeMap::new(),
            tools: BTreeMap::new(),
            errors: BTreeMap::new(),
            mcp_servers: 0,
            compactions: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd_micros: 0,
            update_banner_shown: false,
        }
    }

    /// One agent turn (a full user request → final answer cycle).
    pub fn record_turn(&mut self) {
        self.turns = self.turns.saturating_add(1);
    }

    /// One message added to the conversation.
    pub fn record_message(&mut self) {
        self.messages = self.messages.saturating_add(1);
    }

    /// An LLM request served by `provider` running `model`. Both go through the
    /// sanitizers: allowlisted names pass, anything else is hashed/normalized.
    pub fn record_llm_request(&mut self, provider: &str, model: &str) {
        bump_capped(&mut self.providers, &normalize_provider(provider));
        bump_capped(
            &mut self.models,
            &crate::telemetry::sanitize::hash_identifier(model),
        );
    }

    /// Feature usage (route opened, dialog shown, command run).
    pub fn record_feature(&mut self, feature: Feature) {
        bump_feature(&mut self.features, feature);
    }

    /// A tool call. Unknown tool names collapse into the `other` bucket.
    pub fn record_tool_call(&mut self, tool: &str) {
        bump_capped(&mut self.tools, normalize_tool(tool));
    }

    /// An error occurred. The raw message is fingerprinted and discarded here —
    /// only the representation (category + fingerprint + source) is retained.
    ///
    /// Fail-closed: an invalid `source` (not an internal module path) drops
    /// the record entirely — raw text must never enter the payload.
    pub fn record_error(
        &mut self,
        category: ErrorCategory,
        source: &str,
        provider: Option<&str>,
        raw_message: &str,
    ) {
        let Some(source) = ErrorSource::validate(source) else {
            log::warn!("telemetry: error record dropped (invalid source identifier)");
            return;
        };
        let fingerprint = fingerprint_error(raw_message);
        let key = ErrorKey {
            category,
            provider: provider.map(normalize_provider).unwrap_or_default(),
            source,
            fingerprint,
        };
        if !self.errors.contains_key(&key) && self.errors.len() >= MAX_ERRORS {
            return; // cardinality cap: fail closed
        }
        let slot = self
            .errors
            .entry(key)
            .or_insert(ErrorSlot { occurrence: 0 });
        slot.occurrence = slot.occurrence.saturating_add(1);
    }

    /// Number of MCP servers configured for this session (count only).
    pub fn set_mcp_server_count(&mut self, count: u32) {
        self.mcp_servers = count;
    }

    /// A context compaction happened.
    pub fn record_compaction(&mut self) {
        self.compactions = self.compactions.saturating_add(1);
    }

    /// Real provider-reported usage for a completed LLM request. All adds are
    /// saturating: telemetry counters must never panic (debug overflow checks).
    ///
    /// Cost is accumulated in micro-USD fixed point and rounded to cents ONCE
    /// in [`SessionTelemetry::finish`]. Non-finite or negative
    /// costs are ignored.
    pub fn record_usage(&mut self, tokens_in: u64, tokens_out: u64, cost_usd: Option<f64>) {
        self.tokens_in = self.tokens_in.saturating_add(tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(tokens_out);
        if let Some(cost) = cost_usd.filter(|c| c.is_finite() && *c >= 0.0) {
            let micros = (cost * 1_000_000.0).round() as u64;
            self.cost_usd_micros = self.cost_usd_micros.saturating_add(micros);
        }
    }

    /// The update banner was shown to the user.
    pub fn mark_update_banner(&mut self) {
        self.update_banner_shown = true;
    }

    /// Build the final `session_summary` payload. Consumes the accumulator.
    /// Cost is rounded to cents exactly once, here. The rounding
    /// add is saturating: even a u64::MAX micro-USD accumulator must never
    /// panic or wrap (saturating invariant).
    pub fn finish(self) -> SessionSummaryPayload {
        let cost_usd_cents =
            (self.cost_usd_micros.saturating_add(5_000) / 10_000).min(u32::MAX as u64) as u32;
        let errors = self
            .errors
            .into_iter()
            .filter_map(|(key, slot)| {
                ErrorRepr::new(
                    key.category,
                    &key.fingerprint,
                    key.source,
                    if key.provider.is_empty() {
                        None
                    } else {
                        Some(key.provider)
                    },
                    slot.occurrence,
                )
            })
            .collect();
        SessionSummaryPayload::from_aggregates(
            round_duration(self.started_at.elapsed().as_secs()),
            self.turns,
            self.messages,
            self.providers,
            self.models,
            self.features,
            self.tools,
            self.mcp_servers,
            self.compactions,
            self.tokens_in,
            self.tokens_out,
            cost_usd_cents,
            errors,
            self.update_banner_shown,
        )
    }

    /// Current schema version stamped on outgoing payloads (convenience for
    /// callers building the envelope).
    pub fn schema_version(&self) -> u16 {
        SCHEMA_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_and_never_keeps_raw_error_messages() {
        let mut s = SessionTelemetry::new();
        s.record_turn();
        s.record_message();
        s.record_llm_request("openai", "gpt-4o");
        s.record_llm_request("openai", "gpt-4o");
        s.record_llm_request("weird-internal-provider", "whatever");
        s.record_feature(Feature::Session);
        s.record_tool_call("bash");
        s.record_tool_call("bash");
        s.record_tool_call("totally_unknown_user_tool");
        s.record_error(
            ErrorCategory::ProviderNetwork,
            "harness::core",
            Some("openai"),
            "connection failed to https://internal.acme.com/v1 after 3 tries",
        );
        s.record_error(
            ErrorCategory::ProviderNetwork,
            "harness::core",
            Some("openai"),
            "connection failed to https://other.host.example/v9 after 7 tries",
        );
        s.record_usage(1200, 300, Some(0.0123));
        s.set_mcp_server_count(2);
        s.record_compaction();

        let payload = s.finish();
        assert_eq!(payload.turns(), 1);
        assert_eq!(payload.messages(), 1);
        assert_eq!(payload.providers().get("openai"), Some(&2));
        // Custom provider name is hashed, never shipped raw.
        assert!(!payload.providers().contains_key("weird-internal-provider"));
        assert_eq!(payload.providers().len(), 2);
        // Unknown tools collapse into `other`.
        assert_eq!(payload.tools().get("other"), Some(&1));
        // Two structurally identical errors collapse into one representation.
        assert_eq!(payload.errors().len(), 1);
        let err = &payload.errors()[0];
        assert_eq!(err.occurrence(), 2);
        // The raw message text is nowhere in the serialized payload.
        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("internal.acme.com"));
        assert!(!json.contains("connection failed"));
        assert_eq!(payload.mcp_servers(), 2);
        assert_eq!(payload.compactions(), 1);
        assert_eq!(payload.cost_usd_cents(), 1); // 0.0123 USD fixed-point
    }

    #[test]
    fn cost_is_rounded_once_not_per_request() {
        // 100 × USD 0.004 = USD 0.40 — per-request rounding lost it.
        let mut s = SessionTelemetry::new();
        for _ in 0..100 {
            s.record_usage(1, 1, Some(0.004));
        }
        assert_eq!(s.finish().cost_usd_cents(), 40);
    }

    #[test]
    fn invalid_error_source_is_dropped() {
        let mut s = SessionTelemetry::new();
        // private-looking but syntactically valid identifiers
        // must be REJECTED — the source field is a closed allowlist.
        s.record_error(
            ErrorCategory::FsIo,
            "acme::confidential_merger",
            None,
            "failed",
        );
        s.record_error(ErrorCategory::FsIo, "alice@example.test", None, "failed");
        s.record_error(ErrorCategory::FsIo, "/home/alice/mod.rs", None, "failed");
        assert!(s.finish().errors().is_empty());
    }

    #[test]
    fn errors_group_by_full_dimension_set() {
        // same message, different category/provider/source stay
        // distinct representations.
        let mut s = SessionTelemetry::new();
        s.record_error(
            ErrorCategory::ProviderAuth,
            "harness::a",
            Some("openai"),
            "request failed",
        );
        s.record_error(
            ErrorCategory::ProviderNetwork,
            "harness::b",
            Some("claude"),
            "request failed",
        );
        let payload = s.finish();
        assert_eq!(payload.errors().len(), 2);
        assert_eq!(payload.errors()[0].occurrence(), 1);
        assert_eq!(payload.errors()[1].occurrence(), 1);
    }

    #[test]
    fn duration_is_rounded() {
        // Indirect: round_duration is tested in sanitize; here we just ensure
        // finish() runs without panicking on a fresh accumulator.
        let payload = SessionTelemetry::new().finish();
        assert_eq!(payload.turns(), 0);
    }
}
