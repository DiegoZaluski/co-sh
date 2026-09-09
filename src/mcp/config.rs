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
    /// Extra request headers, e.g. `Authorization`.
    #[serde(default)]
    pub headers: HashMap<String, String>,
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

/// Build a validated entry from wizard fields. Single constructor so the
/// dialog and headless callers share the exact same rules.
pub fn build_mcp_entry(
    name: &str,
    endpoint: &str,
    timeout: &str,
) -> Result<McpServerEntry, String> {
    if name.trim().is_empty() {
        return Err("Server name is required.".to_string());
    }
    let mut transport = parse_mcp_endpoint(endpoint)?;
    if let McpTransport::Http(http) = &mut transport {
        http.timeout_ms = parse_mcp_timeout(timeout)?;
    }
    let entry = McpServerEntry {
        name: name.trim().to_string(),
        transport,
        enabled: true,
    };
    validate_mcp_entry(&entry).map_err(|err| err.to_string())?;
    Ok(entry)
}
