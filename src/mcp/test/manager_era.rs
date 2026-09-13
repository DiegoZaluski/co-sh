//! Era probing and cached-assumption retry ([`super::super::manager`]).
//!
//! Covers the modern probe, legacy fallback, stale-cache recovery in both
//! directions, silent-server policy differences, and version mismatches.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::era::{self, Era};
use super::super::error::McpError;
use super::super::manager::{McpManager, ProbePolicy, era_loop_failed, map_initialize_error};
use super::super::types::ServerStatus;
use super::manager_support::{
    LegacyOnlyServer, ModernOnlyServer, OldVersionServer, SilentServer, attach_server,
    attach_server_full, http_entry, ok_server,
};

#[test]
fn era_loop_failed_reports_both_causes() {
    let first = McpError::EraStale(
        "s".into(),
        Era::Modern.label(),
        "modern refused".into(),
    );
    let second = McpError::EraStale("s".into(), Era::Legacy.label(), "legacy refused".into());
    let err = era_loop_failed("s", Era::Legacy, &first, second);
    let text = err.to_string();
    assert!(text.contains("modern refused"), "{text}");
    assert!(text.contains("legacy refused"), "{text}");
    assert!(text.contains("neither protocol era"), "{text}");
}

/// Regression (mem0): a legacy server that cannot parse `server/discover`
/// answered with a JSON-RPC error whose `id` does not echo the request's —
/// rmcp wraps that as `UncorrelatedErrorResponse` and hides the payload.
/// That classification must count as legacy flip evidence on a modern dial,
/// otherwise the server can never connect (it used to surface as "without
/// era evidence"). On a legacy dial the same garbage stays a plain failure:
/// the modern era was already tried once in that cycle, so a flip is a
/// no-op loop.
#[test]
fn uncorrelated_error_response_is_legacy_evidence_on_modern_dial() {
    use rmcp::model::NumberOrString;
    use rmcp::service::ClientInitializeError;

    let expected = rmcp::model::NumberOrString::Number(0);
    let received = rmcp::model::NumberOrString::Number(42);
    let err = ClientInitializeError::UncorrelatedErrorResponse {
        expected,
        received,
    };

    // Modern assumption → flip evidence (EraStale names the assumed era).
    let mapped = (map_initialize_error("mem0", Era::Modern))(err);
    assert!(
        matches!(mapped, McpError::EraStale(..)),
        "modern dial must flip on an uncorrelated error response, got {mapped:?}"
    );
    assert!(
        mapped.to_string().contains("uncorrelated"),
        "error text must carry the rmcp cause for the toast/log: {mapped}"
    );

    // Legacy assumption → plain failure, no era claim.
    let same = ClientInitializeError::UncorrelatedErrorResponse {
        expected: NumberOrString::Number(0),
        received: NumberOrString::Number(42),
    };
    let mapped = (map_initialize_error("mem0", Era::Legacy))(same);
    assert!(
        matches!(mapped, McpError::Connect(..)),
        "legacy dial must NOT flip on an uncorrelated error response, got {mapped:?}"
    );
}

#[tokio::test]
async fn unknown_server_probes_then_caches_modern_era() {
    let mut manager = McpManager::new();
    let (server, probes) = ok_server("alpha.tool", 0, false);
    attach_server(&mut manager, "a", server).await;

    assert_eq!(manager.era_of("a"), Some(era::Era::Modern));
    assert_eq!(probes.load(Ordering::Relaxed), 1, "first connect must probe");

    // Reconnect the same name: the cached era dials `Discover` directly,
    // skipping the open-ended probe. The one discover round trip left is
    // the lifecycle's own version negotiation, not era detection.
    let (server, probes) = ok_server("alpha.tool", 0, false);
    attach_server(&mut manager, "a", server).await;
    assert_eq!(
        probes.load(Ordering::Relaxed),
        1,
        "cached era dials Discover directly (single negotiation round)"
    );
}

#[tokio::test]
async fn legacy_server_falls_back_to_initialize() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "l",
        LegacyOnlyServer {
            tool: "legacy.tool".into(),
        },
    )
    .await;

    assert_eq!(manager.era_of("l"), Some(super::super::era::Era::Legacy));
    assert_eq!(manager.owner_of("legacy.tool"), Some("l"));
}

#[tokio::test]
async fn stale_legacy_cache_recovers_as_modern() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "s",
        LegacyOnlyServer {
            tool: "old.tool".into(),
        },
    )
    .await;
    assert_eq!(manager.era_of("s"), Some(super::super::era::Era::Legacy));

    // The server was modernized between reconnects: the cached legacy
    // era is rejected (`initialize` is unknown there) and the retry
    // connects modern.
    attach_server(
        &mut manager,
        "s",
        ModernOnlyServer {
            tool: "new.tool".into(),
        },
    )
    .await;
    assert_eq!(manager.era_of("s"), Some(super::super::era::Era::Modern));
    assert_eq!(manager.owner_of("new.tool"), Some("s"));
}

#[tokio::test]
async fn stale_modern_cache_recovers_as_legacy() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "m",
        ModernOnlyServer {
            tool: "new.tool".into(),
        },
    )
    .await;
    assert_eq!(manager.era_of("m"), Some(super::super::era::Era::Modern));

    // The server was downgraded between reconnects: the cached modern
    // era is rejected (`discover` unknown there) and the retry handshakes
    // legacy.
    attach_server(
        &mut manager,
        "m",
        LegacyOnlyServer {
            tool: "old.tool".into(),
        },
    )
    .await;
    assert_eq!(manager.era_of("m"), Some(super::super::era::Era::Legacy));
    assert_eq!(manager.owner_of("old.tool"), Some("m"));
}

#[tokio::test]
async fn transport_failure_keeps_cached_era() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "t",
        LegacyOnlyServer {
            tool: "t.tool".into(),
        },
    )
    .await;
    assert_eq!(manager.era_of("t"), Some(super::super::era::Era::Legacy));

    // A dead transport produces no correlated JSON-RPC rejection, so
    // the era cache must survive the failed connect untouched.
    let entry = http_entry("t", "http://127.0.0.1:1/mcp");
    assert!(manager.connect_one(&entry).await.is_err());
    assert_eq!(manager.era_of("t"), Some(super::super::era::Era::Legacy));
}

#[tokio::test]
async fn silent_stdio_server_falls_back_to_legacy() {
    let mut manager = McpManager::new();
    attach_server_full(
        &mut manager,
        "silent",
        SilentServer {
            tool: "quiet.tool".into(),
        },
        ProbePolicy::STDIO,
        Duration::from_millis(150),
    )
    .await
    .unwrap();

    // Spec, stdio binding: "the probe returns a non-modern error or
    // times out, and the client falls back to initialize".
    assert_eq!(manager.era_of("silent"), Some(era::Era::Legacy));
    assert_eq!(manager.owner_of("quiet.tool"), Some("silent"));
}

#[tokio::test]
async fn silent_probe_never_flips_without_stdio_policy() {
    let mut manager = McpManager::new();
    let outcome = attach_server_full(
        &mut manager,
        "hush",
        SilentServer {
            tool: "quiet.tool".into(),
        },
        // The HTTP policy treats a probe timeout as unreachable, not
        // legacy — a silent stdio binary must not be classified by it.
        ProbePolicy::HTTP,
        Duration::from_millis(150),
    )
    .await;
    assert!(
        matches!(outcome, Err(McpError::ProbeTimedOut(_, 150))),
        "expected a typed probe timeout, got {outcome:?}"
    );

    assert_eq!(manager.era_of("hush"), None);
    assert_eq!(manager.owner_of("quiet.tool"), None);
    let snapshots = manager.status_snapshots();
    let snapshot = snapshots.iter().find(|s| s.name == "hush").unwrap();
    assert!(
        matches!(snapshot.status, ServerStatus::Failed),
        "expected Failed, got {snapshot:?}"
    );
    assert!(snapshot.last_error.is_some());
}

#[tokio::test]
async fn modern_server_without_shared_version_is_typed_failure() {
    let mut manager = McpManager::new();
    let outcome = attach_server_full(
        &mut manager,
        "fut",
        OldVersionServer {
            tool: "old.tool".into(),
        },
        ProbePolicy::STDIO,
        Duration::from_millis(150),
    )
    .await;
    assert!(
        matches!(
            &outcome,
            Err(McpError::Connect(_, text)) if text.contains("compatible protocol version")
        ),
        "expected a version-mismatch diagnostic, got {outcome:?}"
    );

    // Discovery answered, so the server is modern; a version mismatch
    // there is terminal (no fall-forward exists), not a fallback signal.
    assert_eq!(manager.era_of("fut"), None);
    assert_eq!(manager.owner_of("old.tool"), None);
    let snapshots = manager.status_snapshots();
    let snapshot = snapshots.iter().find(|s| s.name == "fut").unwrap();
    assert!(matches!(snapshot.status, ServerStatus::Failed));
    let error = snapshot.last_error.as_deref().unwrap_or_default();
    assert!(
        error.contains("compatible protocol version"),
        "expected a version-mismatch diagnostic, got: {error}"
    );
}
