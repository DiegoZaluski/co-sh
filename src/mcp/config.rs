use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::error::McpError;

/// Timeout applied to HTTP servers when none is configured.
const DEFAULT_HTTP_TIMEOUT_MS: u64 = 30_000;

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
            match url::Url::parse(http.url.trim()) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => {}
                _ => {
                    return Err(McpError::InvalidConfig(
                        entry.name.clone(),
                        "URL must be http(s):// with a host".to_string(),
                    ));
                }
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

/// Whether `url` is an `http(s)://` URL with a host. Shared by entry
/// validation and the registration wizard so both reject the same shapes.
pub fn is_valid_http_url(url: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_entry_roundtrips() {
        let entry = McpServerEntry {
            name: "everything".to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "server".to_string()],
                env: HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: McpServerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
        assert!(validate_mcp_entry(&back).is_ok());
    }

    #[test]
    fn http_entry_applies_timeout_default() {
        let entry: McpServerEntry = serde_json::from_str(
            r#"{"name":"remote","transport":{"type":"http","url":"https://example.com/mcp"}}"#,
        )
        .unwrap();
        match &entry.transport {
            McpTransport::Http(http) => assert_eq!(http.timeout_ms, DEFAULT_HTTP_TIMEOUT_MS),
            McpTransport::Stdio(_) => panic!("expected http transport"),
        }
        assert!(entry.enabled);
        assert!(validate_mcp_entry(&entry).is_ok());
    }

    #[test]
    fn validation_rejects_bad_entries() {
        let blank_name = McpServerEntry {
            name: "  ".to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "npx".to_string(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&blank_name).is_err());

        let blank_command = McpServerEntry {
            name: "s".to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: String::new(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&blank_command).is_err());

        let bad_url = McpServerEntry {
            name: "s".to_string(),
            transport: McpTransport::Http(HttpTransport {
                url: "example.com/mcp".to_string(),
                headers: HashMap::new(),
                timeout_ms: 1000,
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&bad_url).is_err());

        let missing_host = McpServerEntry {
            name: "s".to_string(),
            transport: McpTransport::Http(HttpTransport {
                url: "https://".to_string(),
                headers: HashMap::new(),
                timeout_ms: 1000,
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&missing_host).is_err());

        let zero_timeout = McpServerEntry {
            name: "s".to_string(),
            transport: McpTransport::Http(HttpTransport {
                url: "https://example.com/mcp".to_string(),
                headers: HashMap::new(),
                timeout_ms: 0,
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&zero_timeout).is_err());
    }

    #[test]
    fn disabled_entry_roundtrips() {
        let entry: McpServerEntry = serde_json::from_str(
            r#"{"name":"off","enabled":false,"transport":{"type":"stdio","command":"srv"}}"#,
        )
        .unwrap();
        assert!(!entry.enabled);
        assert!(validate_mcp_entry(&entry).is_ok());
    }

    #[test]
    fn unknown_transport_and_typos_are_rejected() {
        assert!(
            serde_json::from_str::<McpServerEntry>(
                r#"{"name":"s","transport":{"type":"sse","url":"https://example.com"}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<McpServerEntry>(
                r#"{"name":"s","transport":{"type":"stdio","commmand":"srv"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn url_shapes_are_parsed_not_prefix_matched() {
        let valid = [
            "http://localhost:8000/mcp",
            "https://example.com",
            "HTTP://example.com/mcp",
            "  https://example.com/mcp  ",
        ];
        for url in valid {
            let entry = McpServerEntry {
                name: "s".to_string(),
                transport: McpTransport::Http(HttpTransport {
                    url: url.to_string(),
                    headers: HashMap::new(),
                    timeout_ms: 1000,
                }),
                enabled: true,
            };
            assert!(validate_mcp_entry(&entry).is_ok(), "{url}");
        }
        let invalid = [
            "http://",
            "https://",
            "http://?x",
            "ftp://example.com",
            "not a url",
        ];
        for url in invalid {
            let entry = McpServerEntry {
                name: "s".to_string(),
                transport: McpTransport::Http(HttpTransport {
                    url: url.to_string(),
                    headers: HashMap::new(),
                    timeout_ms: 1000,
                }),
                enabled: true,
            };
            assert!(validate_mcp_entry(&entry).is_err(), "{url}");
        }
    }

    #[test]
    fn full_section_rejects_duplicates() {
        let entry = |name: &str| McpServerEntry {
            name: name.to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "srv".to_string(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        };
        assert!(
            validate_mcp_config(&McpConfig {
                servers: vec![entry("a"), entry("a")]
            })
            .is_err()
        );
        assert!(
            validate_mcp_config(&McpConfig {
                servers: vec![entry("a"), entry("b")]
            })
            .is_ok()
        );
        let legacy: McpConfig = serde_json::from_str("{}").unwrap();
        assert!(legacy.servers.is_empty());
        assert!(validate_mcp_config(&legacy).is_ok());
    }
    #[test]
    fn stdio_rejects_blank_cwd_and_env_keys() {
        let entry = McpServerEntry {
            name: "s".to_string(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "srv".to_string(),
                args: vec![],
                env: HashMap::from([("".to_string(), "x".to_string())]),
                cwd: Some("  ".to_string()),
            }),
            enabled: true,
        };
        assert!(validate_mcp_entry(&entry).is_err());
    }

    #[test]
    fn wizard_parses_endpoints_and_timeouts() {
        match parse_mcp_endpoint("npx -y @modelcontextprotocol/server-everything").unwrap() {
            McpTransport::Stdio(stdio) => {
                assert_eq!(stdio.command, "npx");
                assert_eq!(
                    stdio.args,
                    vec!["-y", "@modelcontextprotocol/server-everything"]
                );
            }
            McpTransport::Http(_) => panic!("expected stdio"),
        }
        match parse_mcp_endpoint("https://example.com/mcp").unwrap() {
            McpTransport::Http(http) => assert_eq!(http.url, "https://example.com/mcp"),
            McpTransport::Stdio(_) => panic!("expected http"),
        }
        assert!(parse_mcp_endpoint("   ").is_err());

        assert_eq!(parse_mcp_timeout(""), Ok(DEFAULT_HTTP_TIMEOUT_MS));
        assert_eq!(parse_mcp_timeout("30"), Ok(30_000));
        assert!(parse_mcp_timeout("abc").is_err());
        assert!(parse_mcp_timeout("0").is_err());

        let entry = build_mcp_entry("docs", "https://example.com/mcp", "45").unwrap();
        assert_eq!(entry.name, "docs");
        assert!(entry.enabled);
        match entry.transport {
            McpTransport::Http(http) => assert_eq!(http.timeout_ms, 45_000),
            McpTransport::Stdio(_) => panic!("expected http"),
        }
        assert!(build_mcp_entry("", "npx -y x", "").is_err());
        assert!(build_mcp_entry("s", "", "").is_err());
        assert!(build_mcp_entry("s", "https://example.com", "abc").is_err());
    }
}
