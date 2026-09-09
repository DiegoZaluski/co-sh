//! Protocol-era handling for the MCP client (SEP-2575).
//!
//! The 2026-07-28 spec removed the `initialize` handshake: every request now
//! carries its protocol version, identity, and capabilities as per-request
//! metadata. Servers from the previous era (2025-11-25 and earlier) still
//! expect the legacy handshake. A client that wants to talk to both speaks of
//! two *eras*:
//!
//! - [`Era::Modern`]: per-request metadata, no handshake, `server/discover`
//!   probe.
//! - [`Era::Legacy`]: `initialize` / `notifications/initialized` handshake,
//!   the only behavior of rmcp's plain `serve()`.
//!
//! Detection is deliberately *not* delegated to rmcp's
//! `ClientLifecycleMode::Auto`: its fallback reuses the same transport
//! session, and an rmcp server marks a session whose first message was
//! `discover` as requiring per-request metadata — so the fallback's legacy
//! requests are all rejected. The manager instead dials each era explicitly
//! on a fresh transport (see `McpManager::register_with_retry`). This module
//! owns the era vocabulary: the version preference (pinned explicitly, never
//! `LATEST`, which still points at 2025-11-25 in rmcp 3.2.0), the readback of
//! the era a connection settled in, and the classification of JSON-RPC
//! rejections into era evidence for the probe and the cached-assumption
//! retry.

use std::time::Duration;

use rmcp::model::{ErrorCode, ErrorData, ProtocolVersion, ServerPeerInfo};

/// Which wire dialect a connected server speaks.
///
/// A property of the server (stdio process or HTTP origin), not of an
/// individual request; the manager caches it per server name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    /// Stateless per-request metadata (2026-07-28 and later).
    Modern,
    /// Legacy `initialize` handshake (2025-11-25 and earlier).
    Legacy,
}

impl Era {
    /// Human-readable label for logs and error messages.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Era::Modern => "modern (2026-07-28)",
            Era::Legacy => "legacy (initialize handshake)",
        }
    }

    /// The era to flip to when `self` is refuted.
    pub(crate) const fn other(self) -> Era {
        match self {
            Era::Modern => Era::Legacy,
            Era::Legacy => Era::Modern,
        }
    }
}

/// Budget for the open-ended era probe of an unknown server.
///
/// The spec's stdio binding classifies a probe *timeout* as a legacy
/// signal, so the probe needs its own budget: generous for a real server
/// to boot, but bounded so a silent one falls back promptly. rmcp's
/// internal `Auto` timeout (10 s) is not exposed, and this budget only
/// ever applies to unknown servers — a cached era dials directly with no
/// probing.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Versions we offer a modern server, most preferred first.
///
/// Pinned on purpose: `ProtocolVersion::LATEST` still equals
/// `V_2025_11_25` in rmcp 3.2.0, so an unpinned preference would silently
/// negotiate the legacy revision. Allocates one tiny `Vec` per dial —
/// negligible next to spawning a transport; kept as `Vec` to match the
/// `ClientLifecycleMode::Discover` signature.
pub(crate) fn preferred_versions() -> Vec<ProtocolVersion> {
    vec![ProtocolVersion::V_2026_07_28]
}

/// Read the era back from the connected peer's negotiated info.
///
/// `discover` mode stamps the selected version from `server/discover`
/// (`from_discover_result`); a legacy handshake stamps the initialize
/// result. Everything below `V_2026_07_28` is legacy by definition. A
/// missing peer info is treated as legacy: only the legacy handshake can
/// complete without one being stamped. `>=` relies on `ProtocolVersion`'s
/// lexicographic `PartialOrd` over `YYYY-MM-DD` strings, which orders
/// revisions chronologically; a future `2030-01-01` therefore compares
/// greater than `V_2026_07_28` (covered by
/// `era_readback_treats_future_versions_as_modern`).
pub(crate) fn era_of(peer_info: Option<&ServerPeerInfo>) -> Era {
    match peer_info {
        Some(info) if info.protocol_version >= ProtocolVersion::V_2026_07_28 => Era::Modern,
        _ => Era::Legacy,
    }
}

/// Whether a JSON-RPC error identifies a *modern* server rejecting our
/// request, versus a legacy server that simply does not know the method.
///
/// The reserved modern-era codes (-32022/-32021/-32020, i.e.
/// `UNSUPPORTED_PROTOCOL_VERSION` / `MISSING_REQUIRED_CLIENT_CAPABILITY` /
/// `HEADER_MISMATCH` per the MCP 2026-07-28 versioning table "Backward
/// Compatibility") can only originate from a server speaking the 2026-07-28
/// protocol; anything else (method-not-found, parse errors, implementation
/// codes) is a legacy signal. A future reserved modern code added by a
/// later spec revision would misclassify as legacy until added here.
/// The manager folds this into flip evidence per dial direction —
/// see `map_initialize_error` there.
pub(crate) fn is_modern_error(error: &ErrorData) -> bool {
    matches!(
        error.code,
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
            | ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
            | ErrorCode::HEADER_MISMATCH
    )
}
