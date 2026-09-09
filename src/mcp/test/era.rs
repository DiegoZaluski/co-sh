//! Protocol-era vocabulary ([`super::super::era`]).
//!
//! Short and correlated enough to stay in one file: version pinning, era
//! readback, and modern-error classification.

use rmcp::model::{
    DiscoverResult, ErrorCode, ErrorData, ProtocolVersion, ServerCapabilities, ServerPeerInfo,
};
use rmcp::service::select_protocol_version;

use super::super::era::{Era, era_of, is_modern_error, preferred_versions};

fn peer_info(version: ProtocolVersion) -> ServerPeerInfo {
    let result = DiscoverResult::new(vec![version.clone()], ServerCapabilities::default());
    ServerPeerInfo::from_discover_result(version, result)
}

#[test]
fn preferred_versions_pin_2026_07_28() {
    // Never `LATEST`, which still points at 2025-11-25 in rmcp 3.2.0.
    assert_eq!(preferred_versions(), vec![ProtocolVersion::V_2026_07_28]);
}

#[test]
fn era_readback_distinguishes_modern_and_legacy() {
    assert_eq!(
        era_of(Some(&peer_info(ProtocolVersion::V_2026_07_28))),
        Era::Modern
    );
    assert_eq!(
        era_of(Some(&peer_info(ProtocolVersion::V_2025_11_25))),
        Era::Legacy
    );
    // Only the legacy handshake can complete without peer info.
    assert_eq!(era_of(None), Era::Legacy);
}

#[test]
fn modern_errors_are_exactly_the_reserved_codes() {
    let modern = [
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
        ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY,
        ErrorCode::HEADER_MISMATCH,
    ];
    for code in modern {
        let error = ErrorData::new(code, "rejected", None);
        assert!(is_modern_error(&error), "{code:?} must be modern");
    }

    let legacy = [
        ErrorCode::METHOD_NOT_FOUND,
        ErrorCode::PARSE_ERROR,
        ErrorCode::INTERNAL_ERROR,
        ErrorCode(-32000), // implementation-defined range
    ];
    for code in legacy {
        let error = ErrorData::new(code, "legacy server", None);
        assert!(!is_modern_error(&error), "{code:?} must be legacy");
    }
}

#[test]
fn version_selection_requires_an_exact_match() {
    assert_eq!(
        select_protocol_version(&preferred_versions(), &[ProtocolVersion::V_2026_07_28]),
        Some(ProtocolVersion::V_2026_07_28)
    );
    // A future version we do not offer must not match: unknown versions
    // deserialize as opaque strings, never equal to a known constant.
    let future: ProtocolVersion = serde_json::from_str("\"2030-01-01\"").unwrap();
    assert_eq!(
        select_protocol_version(&preferred_versions(), &[future]),
        None
    );
    assert_eq!(select_protocol_version(&preferred_versions(), &[]), None);
}

#[test]
fn era_readback_treats_future_versions_as_modern() {
    let future: ProtocolVersion = serde_json::from_str("\"2030-01-01\"").unwrap();
    assert_eq!(era_of(Some(&peer_info(future))), Era::Modern);
}

#[test]
fn era_labels_are_stable() {
    assert_eq!(Era::Modern.label(), "modern (2026-07-28)");
    assert_eq!(Era::Legacy.label(), "legacy (initialize handshake)");
}
