//! Registration-wizard parsing ([`super::super::config`]).
//!
//! Covers the endpoint/timeout fields and the validated entry constructor
//! shared by the dialog and headless callers.

use super::super::config::{
    DEFAULT_HTTP_TIMEOUT_MS, McpTransport, build_mcp_entry, parse_mcp_endpoint, parse_mcp_timeout,
};

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
