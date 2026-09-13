//! Config validation and serialization ([`super::super::config`]).
//!
//! Covers entry/section validation, serde round-trips and defaults, URL
//! shape acceptance, and duplicate-name rejection.

use std::collections::HashMap;

use super::super::config::{
    DEFAULT_HTTP_TIMEOUT_MS, HttpTransport, McpConfig, McpServerEntry, McpTransport,
    StdioTransport, validate_mcp_config, validate_mcp_entry,
};

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
            api_key_env: None,
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
            api_key_env: None,
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
            api_key_env: None,
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
            api_key_env: None,
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
            api_key_env: None,
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
