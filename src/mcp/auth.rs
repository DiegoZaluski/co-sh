//! MCP HTTP credential resolution (hybrid keyring/environment).
//!
//! A registered HTTP server may need an API key (sent as
//! `Authorization: Bearer <key>`, the spec's access-token usage). The
//! credential is resolved at dial time with a strict priority:
//!
//! 1. **OS keyring** — the credential the registration wizard stored under
//!    the cosh service, keyed `mcp:<server>` (same store as provider keys).
//!    This is the primary source: it never touches `setup.json` or process
//!    environments.
//! 2. **Environment variable** — the entry's `api_key_env` (set with
//!    `$VAR` in the wizard or by hand in `setup.json`), consulted only when
//!    the keyring holds nothing.
//! 3. **Typed failure** — when the entry declares `api_key_env` but neither
//!    source yields a value, [`resolve_key`] returns
//!    [`McpError::CredentialMissing`]: the server is skipped with a
//!    `Failed` snapshot instead of dialing into a guaranteed 401.
//!
//! Values are never logged; header errors redact them (see `http_config`).

use super::error::McpError;

/// Keyring service shared with provider credentials.
const SERVICE: &str = cosh_sdk::connector::COSH_SERVICE;

/// Keyring account for one MCP server's API key. A distinct `mcp:` prefix
/// keeps MCP credentials out of the provider-key namespace (`ENV_VAR`-keyed)
/// so a server named like a provider env var can never collide.
fn keyring_id(server_name: &str) -> String {
    format!("mcp:{server_name}")
}

/// Resolve the API key for `server_name` at dial time: keyring first, then
/// the entry's environment variable. `Ok(None)` means "no credential
/// needed" — the entry declares neither a stored key nor `api_key_env`.
///
/// # Errors
///
/// [`McpError::CredentialMissing`] when `api_key_env` is configured but
/// neither the keyring nor the environment provides a value. A keyring
/// *lookup* failure (locked store, dbus error) degrades to the env fallback
/// instead of failing the connection — the variable may still be set.
pub fn resolve_key(
    server_name: &str,
    api_key_env: Option<&str>,
) -> Result<Option<String>, McpError> {
    match stored_key(server_name) {
        Ok(Some(key)) => return Ok(Some(key)),
        // NoEntry is the normal "not stored here" case: fall through to
        // the env fallback silently. Any OTHER keyring error (locked
        // store, dbus failure) is logged redacted — the error type only,
        // never a value — then degrades to the env fallback, which may
        // still succeed.
        Err(err) => {
            log::debug!(
                "MCP keyring lookup for server '{server_name}' failed ({}); \
                 falling back to the environment variable",
                err
            );
        }
        Ok(None) => {}
    }
    if let Some(var) = api_key_env {
        match std::env::var(var.trim()) {
            Ok(key) if !key.trim().is_empty() => return Ok(Some(key)),
            _ => {
                return Err(McpError::CredentialMissing(
                    server_name.to_string(),
                    var.trim().to_string(),
                ));
            }
        }
    }
    Ok(None)
}

/// The stored keyring credential for a server, if the store has one.
/// Store access errors read as "absent" (see [`resolve_key`]).
fn stored_key(server_name: &str) -> Result<Option<String>, keyring::Error> {
    let entry = keyring::Entry::new(SERVICE, &keyring_id(server_name))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Store (or overwrite) the server's API key in the OS keyring. The value
/// never reaches `setup.json` — callers drop it right after this returns.
pub fn store_key(server_name: &str, key: &str) -> Result<(), keyring::Error> {
    keyring::Entry::new(SERVICE, &keyring_id(server_name))?.set_password(key)
}

/// Remove the server's stored credential through the keyring's own
/// deletion API. Missing credentials succeed (idempotent forget).
pub fn forget_key(server_name: &str) -> Result<(), keyring::Error> {
    match keyring::Entry::new(SERVICE, &keyring_id(server_name)) {
        Ok(entry) => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err),
        },
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err),
    }
}
