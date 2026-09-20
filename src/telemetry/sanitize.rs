//! Sanitization: the single exit boundary for telemetry data.
//!
//! Every string field passes [`sanitize_string`] before entering an event, and
//! every event passes [`sanitize_event`] right before enqueueing. Invariant
//! tests (`tests.rs`) assert that paths, URLs, keys and prompt content cannot
//! escape — the field is dropped (with a local warning) rather than rewritten.

use crate::telemetry::schema::{
    ErrorCategory, EventType, FEATURE_ALLOWLIST, Feature, PROVIDER_ALLOWLIST, TOOL_ALLOWLIST,
};
use xxhash_rust::xxh64::xxh64;

/// Forbidden patterns for any string leaving the client. A match means the
/// value could identify a user, a machine or a project — the field is dropped.
const FORBIDDEN_PATTERNS: &[&str] = &[
    "/home/",  // unix home dir
    "/users/", // macOS home dir (matched against the lowercased value)
    "/root/",  // root home dir
    "c:\\",    // windows drive paths (checked against the lowercased value)
    "~/.ssh",  // ssh material
    "id_rsa",
    "localhost", // local infra
    ".local",    // mDNS-style hostnames (with or without port)
    ".internal", // internal DNS domains
    "127.0.0.1", // loopback (v4 and v6)
    "::1",
    "fe80::", // v6 link-local
    "0.0.0.0",
    "192.168.", // private ranges
    "172.16.",
    "172.17.",
    "172.18.",
    "172.19.",
    "172.2", // covers 172.20-172.29 prefixes coarsely
    "172.30.",
    "172.31.",
    "169.254.", // link-local
    "10.",      // full 10.0.0.0/8 (a version string like "10.1.0" is
    // never a payload value at this boundary — all numeric
    // fields are integers, not strings)
    "sk-", // API key shapes
    "ghp_",
    "github_pat_",
    "xox",
    "akia",
    "eyj", // JWT prefix (base64 of {" — lowercase form)
    "bearer ",
    "-----begin ", // PEM blocks (lowercased)
    "postgresql://",
    "http://",
    "https://",
];

/// Hash a free-form identifier into an anonymous grouping id.
///
/// Used for model names outside the known catalog and (in the future, per the
/// plan §4.6) MCP server names. The same well-known identifier always produces
/// the same hash across users, so aggregate rankings still work.
pub fn hash_identifier(raw: &str) -> String {
    let digest = xxh64(raw.trim().to_lowercase().as_bytes(), 0);
    format!("{digest:016x}")
}

/// Fingerprint an error message WITHOUT storing it.
///
/// Normalization: lowercase → strip digits, path-like segments, URLs and
/// quoted/backticked content → hash. Identical errors across thousands of
/// users collapse into the same fingerprint; raw text never leaves the client.
pub fn fingerprint_error(message: &str) -> String {
    let mut normalized = message.to_lowercase();

    // Drop anything that looks like a URL — these carry hosts and user content.
    for token in ["http://", "https://", "file://"] {
        if let Some(idx) = normalized.find(token) {
            normalized.truncate(idx);
        }
    }

    // 1) Strip quoted/backticked regions WHOLE (their whitespace included), so
    // a space inside a quote cannot re-enable capture.
    let mut no_quotes = String::with_capacity(normalized.len());
    let mut quote: Option<char> = None;
    for ch in normalized.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None; // closing quote: region ends
            }
            continue; // everything inside quotes is stripped
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        no_quotes.push(ch);
    }

    // 2) Drop path-like tokens (any token containing / or \\ — this covers
    // spaced paths like "/etc/my config/x", where every fragment still
    // contains a separator) and strip digits from the remaining words.
    let mut words: Vec<String> = Vec::new();
    for token in no_quotes.split_whitespace() {
        if token.contains('/') || token.contains('\\') {
            continue;
        }
        let stripped: String = token.chars().filter(|c| !c.is_numeric()).collect();
        if !stripped.is_empty() {
            words.push(stripped);
        }
    }
    let digest = xxh64(words.join(" ").as_bytes(), 0);
    format!("{digest:016x}")
}

/// Normalize a provider name: allowlisted names pass through, anything else is
/// hashed (custom provider names can embed identity).
pub fn normalize_provider(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if PROVIDER_ALLOWLIST.contains(&lower.as_str()) {
        lower
    } else {
        hash_identifier(&lower)
    }
}

/// Normalize a tool name: allowlisted names pass through, anything else falls
/// into the `other` bucket (never shipped raw).
pub fn normalize_tool(name: &str) -> &'static str {
    let lower = name.trim().to_lowercase();
    if TOOL_ALLOWLIST.contains(&lower.as_str()) {
        // Safe: the name is in TOOL_ALLOWLIST.
        TOOL_ALLOWLIST
            .iter()
            .find(|t| **t == lower)
            .copied()
            .unwrap_or("other")
    } else {
        "other"
    }
}

/// Guard a string field. Returns `None` when the value matches a forbidden
/// pattern (caller drops the field and logs locally).
pub fn sanitize_string(value: &str) -> Option<&str> {
    let lower = value.to_lowercase();
    for pattern in FORBIDDEN_PATTERNS {
        if lower.contains(pattern) {
            return None;
        }
    }
    Some(value)
}

/// Round a duration in seconds down to a multiple of 30 (re-identification
/// resistance while keeping analysis useful).
pub fn round_duration(secs: u64) -> u64 {
    (secs / 30) * 30
}

/// Validate that an event envelope is safe to enqueue. Returns false when any
/// string field fails [`sanitize_string`].
pub fn sanitize_event(type_: EventType, payload_json: &str) -> bool {
    let _ = type_;
    // Payload is built exclusively from allowlisted enums and this module's
    // normalizers; the final guard checks the serialized payload for any
    // forbidden fragment as a belt-and-braces net.
    sanitize_string(payload_json).is_some()
}

/// Belt-and-braces guard over an ALREADY serialized envelope (the
/// sink re-runs the denylist over persisted data before sending).
pub fn validate_serialized(serialized: &str) -> bool {
    sanitize_string(serialized).is_some()
}

/// True when a value is a normalized identifier as produced by this module's
/// normalizers: an allowlisted provider/tool name or a 16-hex-digit hash.
/// Used to re-validate deserialized payload fields.
pub fn is_normalized_identifier(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    PROVIDER_ALLOWLIST.contains(&value)
        || TOOL_ALLOWLIST.contains(&value)
        || FEATURE_ALLOWLIST.contains(&value)
        || (value.len() == 16 && value.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Convenience mapping used by the harness instrumentation (P2). Kept here so
/// the categorization logic lives with the schema, not at call sites.
pub fn categorize_error(kind: &str) -> ErrorCategory {
    match kind {
        "auth" => ErrorCategory::ProviderAuth,
        "rate_limit" => ErrorCategory::ProviderRateLimit,
        "server" => ErrorCategory::ProviderServer,
        "network" => ErrorCategory::ProviderNetwork,
        "context_overflow" => ErrorCategory::ContextOverflow,
        "tool_failed" => ErrorCategory::ToolFailed,
        "fs_io" => ErrorCategory::FsIo,
        "render_panic" => ErrorCategory::RenderPanic,
        "panic" => ErrorCategory::Panic,
        _ => ErrorCategory::Unknown,
    }
}

/// Features are enums already; this exists for symmetry and for callers that
/// only have a string (e.g. from a route id).
pub fn feature_from_str(name: &str) -> Feature {
    match name.trim().to_lowercase().as_str() {
        "session" => Feature::Session,
        "home" => Feature::Home,
        "settings" => Feature::Settings,
        "tools" => Feature::Tools,
        "rag" => Feature::Rag,
        "add_provider" => Feature::AddProvider,
        "prompt" => Feature::Prompt,
        _ => Feature::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_pass_through() {
        assert_eq!(normalize_provider("openai"), "openai");
        assert_eq!(normalize_provider("OpenAI"), "openai");
    }

    #[test]
    fn custom_providers_are_hashed_never_shipped_raw() {
        let h = normalize_provider("acme-corp-proxy");
        assert_ne!(h, "acme-corp-proxy");
        assert_eq!(h.len(), 16);
        // deterministic
        assert_eq!(h, normalize_provider("acme-corp-proxy"));
    }

    #[test]
    fn unknown_tools_fall_into_other() {
        assert_eq!(normalize_tool("bash"), "bash");
        assert_eq!(normalize_tool("user_internal_tool"), "other");
    }

    #[test]
    fn forbidden_patterns_are_caught() {
        assert!(sanitize_string("/home/inky/secret").is_none());
        assert!(sanitize_string("https://internal.acme.com").is_none());
        assert!(sanitize_string("sk-abcdef123").is_none());
        assert!(sanitize_string("C:\\Users\\joão").is_none());
        assert!(sanitize_string("plain text ok").is_some());
    }

    #[test]
    fn fingerprint_is_stable_and_strips_sensitive_fragments() {
        let a = fingerprint_error(
            "Failed to reach https://api.acme.com/v1: 500 (internal) after 3 retries",
        );
        let b = fingerprint_error(
            "failed to reach https://api.other.com/v2: 503 (oops) after 9 retries",
        );
        // Same structure → same fingerprint, raw URLs/digits gone from the hash input.
        assert_eq!(a, b);
        // Deterministic.
        assert_eq!(
            a,
            fingerprint_error(
                "Failed to reach https://api.acme.com/v1: 500 (internal) after 3 retries"
            )
        );
    }

    #[test]
    fn fingerprint_strips_quoted_content_including_spaces() {
        // The quoted phrase (and its words) must NOT reach the hash input: a
        // quote region is stripped whole, whitespace inside it included.
        let a = fingerprint_error("error: \"secret phrase here\" failed");
        let b = fingerprint_error("error: \"completely different words\" failed");
        assert_eq!(a, b, "quoted content must not influence the fingerprint");
        // Backtick content is stripped too: the stripped input normalizes to
        // the same cleaned string as if the quote never existed.
        let c = fingerprint_error("cannot run `rm -rf /home/user` now");
        assert_eq!(c, fingerprint_error("cannot run  now"));
        // A spaced path stays stripped.
        assert_eq!(
            fingerprint_error("failed reading /etc/my config/x file"),
            fingerprint_error("failed reading /var/other/y file")
        );
    }

    #[test]
    fn durations_round_to_30s_multiples() {
        assert_eq!(round_duration(0), 0);
        assert_eq!(round_duration(29), 0);
        assert_eq!(round_duration(31), 30);
        assert_eq!(round_duration(1870), 1860);
    }
}
