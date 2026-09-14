use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::error::McpError;

/// Timeout applied to HTTP servers when none is configured.
/// Visible to the scoped `test` module for default assertions.
pub(crate) const DEFAULT_HTTP_TIMEOUT_MS: u64 = 30_000;

/// Upper bound for the configured HTTP timeout (one hour); larger values are
/// almost certainly a units mistake (seconds written as milliseconds).
const MAX_HTTP_TIMEOUT_MS: u64 = 3_600_000;

/// Transport used to reach one MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

/// Spawn a local server binary and talk over its stdio. Relative commands
/// resolve via `PATH`; `args`/`env` mirror the process spawn exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StdioTransport {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Child working directory (`None` inherits ours).
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Talk to a remote server over Streamable HTTP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTransport {
    pub url: String,
    /// Extra request headers, e.g. `Authorization` (static values only —
    /// never put a credential here; use [`Self::api_key_env`] or the
    /// registration wizard so the key lives in the OS keyring). A resolved
    /// keyring/`api_key_env` credential replaces any static `Authorization`
    /// header configured here on the dial.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Environment variable consulted for the server's API key when the OS
    /// keyring holds no credential (see [`super::auth`]). `None` means the
    /// server is expected to be reachable without a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Per-request timeout in milliseconds.
    #[serde(default = "default_http_timeout_ms")]
    pub timeout_ms: u64,
}

/// One registered MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerEntry {
    /// Unique display name; also keys runtime state.
    pub name: String,
    pub transport: McpTransport,
    /// Disabled servers are skipped at startup without error.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Whole `mcp` section of the user config. Defaults to empty so legacy
/// config files without the section keep loading unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

const fn default_enabled() -> bool {
    true
}

const fn default_http_timeout_ms() -> u64 {
    DEFAULT_HTTP_TIMEOUT_MS
}

/// Reject an entry that could never connect, with a human-readable reason.
pub fn validate_mcp_entry(entry: &McpServerEntry) -> Result<(), McpError> {
    if entry.name.trim().is_empty() {
        return Err(McpError::InvalidConfig(
            entry.name.clone(),
            "server name is required".to_string(),
        ));
    }
    match &entry.transport {
        McpTransport::Stdio(stdio) => {
            if stdio.command.trim().is_empty() {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    "command is required".to_string(),
                ));
            }
            if stdio
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.trim().is_empty())
            {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    "cwd must be a directory or omitted".to_string(),
                ));
            }
            if stdio.env.keys().any(|key| key.trim().is_empty()) {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    "env keys must not be blank".to_string(),
                ));
            }
            Ok(())
        }
        McpTransport::Http(http) => {
            if !is_valid_http_url(&http.url) {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    "URL must be http(s):// with a host".to_string(),
                ));
            }
            if http
                .api_key_env
                .as_deref()
                .is_some_and(|var| !is_env_var_name(var.trim()))
            {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    "api_key_env must name a valid environment variable".to_string(),
                ));
            }
            if http.timeout_ms == 0 || http.timeout_ms > MAX_HTTP_TIMEOUT_MS {
                return Err(McpError::InvalidConfig(
                    entry.name.clone(),
                    format!("timeout must be 1..={MAX_HTTP_TIMEOUT_MS}ms"),
                ));
            }
            Ok(())
        }
    }
}

/// Whether `url` is an `http(s)://` URL with a host. Single definition of
/// the accepted shape, shared by entry validation.
fn is_valid_http_url(url: &str) -> bool {
    match url::Url::parse(url.trim()) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https") && parsed.has_host(),
        Err(_) => false,
    }
}

/// Validate a whole section: every entry plus duplicate server names, which
/// would otherwise collide as manager keys.
pub fn validate_mcp_config(config: &McpConfig) -> Result<(), McpError> {
    let mut seen = HashSet::new();
    for entry in &config.servers {
        if !seen.insert(entry.name.clone()) {
            return Err(McpError::InvalidConfig(
                entry.name.clone(),
                "duplicate server name".to_string(),
            ));
        }
        validate_mcp_entry(entry)?;
    }
    Ok(())
}

/// Parse the timeout field of the registration wizard (seconds): blank
/// means the default, otherwise a plain number of seconds.
pub fn parse_mcp_timeout(input: &str) -> Result<u64, String> {
    const INVALID: &str = "Timeout must be a number of seconds.";
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_HTTP_TIMEOUT_MS);
    }
    let secs: u64 = trimmed.parse().map_err(|_| INVALID.to_string())?;
    let ms = secs.saturating_mul(1000);
    if ms == 0 || ms > MAX_HTTP_TIMEOUT_MS {
        return Err(format!(
            "Timeout must be 1..={}s.",
            MAX_HTTP_TIMEOUT_MS / 1000
        ));
    }
    Ok(ms)
}

/// A valid environment-variable name: `IDENT` characters, not digit-led.
fn is_env_var_name(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The registration wizard's API-key field, classified. A `$VAR` reference
/// names an environment variable (kept in the config); anything else is a
/// directly typed key (returned to the caller for keyring storage — it
/// never enters the config).
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialInput {
    /// No credential configured.
    None,
    /// `$VAR`: resolve from the environment at dial time.
    EnvVar(String),
    /// A directly typed key: the caller stores it in the OS keyring.
    Secret(String),
}

// Manual Debug: `Secret` values are credentials — a stray `{:?}` on a
// draft or form state must never print the key itself.
impl std::fmt::Debug for CredentialInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialInput::None => f.write_str("None"),
            CredentialInput::EnvVar(var) => f.debug_tuple("EnvVar").field(var).finish(),
            CredentialInput::Secret(_) => f.write_str("Secret(\"…\")"),
        }
    }
}

/// Classify the registration wizard's API-key field. Blank means none;
/// `$NAME` names an environment variable (validated); anything else is a
/// secret — trimmed, and rejected outright when it contains control
/// characters, which could never survive as a header value anyway.
pub fn parse_mcp_credential(input: &str) -> Result<CredentialInput, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(CredentialInput::None);
    }
    if let Some(var) = trimmed.strip_prefix('$') {
        let var = var.trim();
        if !is_env_var_name(var) {
            return Err(format!(
                "“{var}” is not a valid environment variable name after '$'."
            ));
        }
        return Ok(CredentialInput::EnvVar(var.to_string()));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("API key must not contain control characters.".to_string());
    }
    Ok(CredentialInput::Secret(trimmed.to_string()))
}

/// Parse the endpoint field of the registration wizard: an `http(s)://` URL
/// becomes a remote server, anything else a `command args...` stdio line.
pub fn parse_mcp_endpoint(endpoint: &str) -> Result<McpTransport, String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err("Endpoint is required.".to_string());
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(McpTransport::Http(HttpTransport {
            url: trimmed.to_string(),
            headers: HashMap::new(),
            api_key_env: None,
            timeout_ms: DEFAULT_HTTP_TIMEOUT_MS,
        }));
    }
    let mut parts = trimmed.split_whitespace();
    let command = parts.next().expect("non-empty input has a first word");
    Ok(McpTransport::Stdio(StdioTransport {
        command: command.to_string(),
        args: parts.map(str::to_string).collect(),
        env: HashMap::new(),
        cwd: None,
    }))
}

/// One validated server draft plus any directly typed secret. `entry` is
/// config-safe (no secret inside — serialize and persist it freely);
/// `secret` must go to the OS keyring via [`super::auth::store_key`] and is
/// then dropped, never written to `setup.json`.
#[derive(Clone, PartialEq)]
pub struct McpEntryDraft {
    pub entry: McpServerEntry,
    pub secret: Option<String>,
}

// Manual Debug: `secret` is a credential — never print it.
impl std::fmt::Debug for McpEntryDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEntryDraft")
            .field("entry", &self.entry)
            .field("secret", &self.secret.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Build a validated entry from wizard fields. Single constructor so the
/// dialog and headless callers share the exact same rules. `credential` is
/// the API-key field: `$VAR` names an environment variable (kept in the
/// entry as `api_key_env`), a literal key comes back as the draft's
/// `secret` for keyring storage.
pub fn build_mcp_entry(
    name: &str,
    endpoint: &str,
    timeout: &str,
    credential: &str,
) -> Result<McpEntryDraft, String> {
    if name.trim().is_empty() {
        return Err("Server name is required.".to_string());
    }
    // The name keys the keyring entry (`mcp:<name>`), shows up in toasts,
    // logs and error messages, and must survive a round-trip through
    // setup.json — so control characters are rejected outright (they could
    // forge log lines or toast line breaks; a trimmed non-empty name is
    // otherwise free-form).
    if name.chars().any(char::is_control) {
        return Err("Server name must not contain control characters.".to_string());
    }
    let parsed_credential = parse_mcp_credential(credential)?;
    let mut transport = parse_mcp_endpoint(endpoint)?;
    match (&mut transport, &parsed_credential) {
        (McpTransport::Http(http), CredentialInput::EnvVar(var)) => {
            http.api_key_env = Some(var.clone());
        }
        // Per the spec, stdio servers take credentials from their own
        // process environment (`env` map), not from a client header.
        (McpTransport::Stdio(_), CredentialInput::None) => {}
        (McpTransport::Stdio(_), _) => {
            return Err("API key applies to http(s):// endpoints only; pass stdio \
                 credentials through the server's own environment."
                .to_string());
        }
        (McpTransport::Http(_), _) => {}
    }
    if let McpTransport::Http(http) = &mut transport {
        http.timeout_ms = parse_mcp_timeout(timeout)?;
    }
    let entry = McpServerEntry {
        name: name.trim().to_string(),
        transport,
        enabled: true,
    };
    validate_mcp_entry(&entry).map_err(|err| err.to_string())?;
    let secret = match parsed_credential {
        CredentialInput::Secret(key) => Some(key),
        _ => None,
    };
    Ok(McpEntryDraft { entry, secret })
}
