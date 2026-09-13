//! Scoped unit tests for credential parsing and dial-time resolution.
//!
//! Keyring-touching paths (`store_key`/`forget_key`/`stored_key`) are
//! intentionally NOT covered here: the keyring in CI is a real OS store and
//! tests must never write credentials to it. The store-backed halves are
//! one-line `keyring::Entry` calls reviewed by hand; everything around them
//! (parsing, priority, fallback, typed failure) is pure and covered below.

use super::super::auth::resolve_key;
use super::super::config::{CredentialInput, parse_mcp_credential};

/// Serializes the env-var tests: `std::env` is process-global and cargo runs
/// test threads in parallel. One mutex over all of them is simpler and
/// safer than trying to be clever per-variable.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Credential parsing (wizard field) ────────────────────────────

#[test]
fn credential_parses_blank_env_and_secret() {
    assert_eq!(parse_mcp_credential("").unwrap(), CredentialInput::None);
    assert_eq!(parse_mcp_credential("   ").unwrap(), CredentialInput::None);

    match parse_mcp_credential(" $MY_KEY ").unwrap() {
        CredentialInput::EnvVar(var) => assert_eq!(var, "MY_KEY"),
        other => panic!("expected env var, got {other:?}"),
    }
    // The typical provider-style names must survive intact.
    match parse_mcp_credential("$MCP_AUTH_TOKEN").unwrap() {
        CredentialInput::EnvVar(var) => assert_eq!(var, "MCP_AUTH_TOKEN"),
        other => panic!("expected env var, got {other:?}"),
    }

    match parse_mcp_credential("  sk-abc123  ").unwrap() {
        CredentialInput::Secret(key) => assert_eq!(key, "sk-abc123"),
        other => panic!("expected secret, got {other:?}"),
    }
}

#[test]
fn credential_rejects_bad_env_names_and_control_chars() {
    // Not identifiers after '$'.
    assert!(parse_mcp_credential("$9KEY").is_err(), "digit-led name");
    assert!(parse_mcp_credential("$MY KEY").is_err(), "space in name");
    assert!(parse_mcp_credential("$KEY-x").is_err(), "hyphen in name");
    assert!(parse_mcp_credential("$").is_err(), "bare dollar");
    // Control characters could never survive as a header value.
    assert!(parse_mcp_credential("sk-\nabc").is_err());
    assert!(parse_mcp_credential("sk-\tabc").is_err());
}

// ── Dial-time resolution (keyring absent in tests → env fallback) ──

#[test]
fn resolve_prefers_env_when_keyring_holds_nothing() {
    let _guard = ENV_LOCK.lock().unwrap();
    // No keyring credential exists for this test-only name (the store is
    // not touched), so the env var decides.
    // SAFETY: test-only, serialized by ENV_LOCK (process-global env).
    unsafe { std::env::set_var("COSH_TEST_MCP_KEY", "from-env") };
    let resolved = resolve_key("cred-test-env", Some("COSH_TEST_MCP_KEY")).unwrap();
    unsafe { std::env::remove_var("COSH_TEST_MCP_KEY") };
    assert_eq!(resolved.as_deref(), Some("from-env"));
}

#[test]
fn resolve_missing_env_is_a_typed_failure() {
    let _guard = ENV_LOCK.lock().unwrap();
    // SAFETY: single-threaded env access under ENV_LOCK (process-global).
    unsafe { std::env::remove_var("COSH_TEST_MCP_KEY_MISSING") };
    let err = resolve_key("cred-test-missing", Some("COSH_TEST_MCP_KEY_MISSING")).unwrap_err();
    assert!(
        err.to_string().contains("COSH_TEST_MCP_KEY_MISSING"),
        "error must name the variable so the user can fix it: {err}"
    );
    // The keyring lookup failure path must NOT mask the env absence.
    assert!(matches!(
        err,
        super::super::error::McpError::CredentialMissing(_, _)
    ));
}

#[test]
fn resolve_without_declared_credential_is_none() {
    // Neither keyring nor env: a public server just dials without a header.
    assert_eq!(resolve_key("cred-test-public", None).unwrap(), None);
}

#[test]
fn resolve_blank_env_value_counts_as_missing() {
    let _guard = ENV_LOCK.lock().unwrap();
    // SAFETY: single-threaded env access under ENV_LOCK (process-global).
    unsafe { std::env::set_var("COSH_TEST_MCP_KEY_BLANK", "   ") };
    let err = resolve_key("cred-test-blank", Some("COSH_TEST_MCP_KEY_BLANK")).unwrap_err();
    unsafe { std::env::remove_var("COSH_TEST_MCP_KEY_BLANK") };
    assert!(matches!(
        err,
        super::super::error::McpError::CredentialMissing(_, _)
    ));
}
