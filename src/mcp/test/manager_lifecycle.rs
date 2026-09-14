//! Connection lifecycle ([`super::super::manager::McpManager`]).
//!
//! Covers `connect_all`/`connect_one` reconciliation, disconnect/evict,
//! snapshots, transport setup (headers, stderr logs), and config gating.

use std::collections::HashMap;

use super::super::config::{
    HttpTransport, McpConfig, McpServerEntry, McpTransport, StdioTransport,
};
use super::super::era;
use super::super::manager::{McpManager, http_config, stderr_log_path};
use super::super::types::ServerStatus;
use super::manager_support::{CatalogServer, attach, attach_server, http_entry};

#[test]
fn stderr_log_path_is_sanitized_and_inside_temp_cosh_log() {
    let path = stderr_log_path("my server/v2");
    let expected_dir = crate::harness::truncate::scratch_log_dir().join("log");
    assert_eq!(path.parent(), Some(expected_dir.as_path()));
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("mcp_server_my_server_v2_") && name.ends_with(".log"),
        "unexpected file name: {name}"
    );
    assert!(!name.contains('/'));
}

#[tokio::test]
async fn evict_clears_cached_era() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "cat",
        CatalogServer {
            tool: "cat.tool".into(),
        },
    )
    .await;
    assert!(manager.era_of("cat").is_some());
    manager.evict("cat").await;
    assert_eq!(manager.era_of("cat"), None);
    assert!(manager.all_resources().is_empty());
}

/// Live end-to-end connect against the official MCP test server over a
/// real stdio child. Ignored by default — run explicitly with
/// `cargo test --lib -- --ignored live_connects`: needs `npx` on PATH
/// and downloads the package on first run. Exercises the exact
/// production path (`connect_all` → `spawn_stdio` → era probe), which
/// the duplex-based tests above never touch.
#[tokio::test]
#[ignore = "live: spawns npx and downloads @modelcontextprotocol/server-everything"]
async fn live_connects_server_everything_over_stdio() {
    let mut manager = McpManager::new();
    let config = McpConfig {
        servers: vec![McpServerEntry {
            name: "everything".into(),
            transport: McpTransport::Stdio(StdioTransport {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-everything".into(),
                ],
                env: HashMap::new(),
                cwd: None,
            }),
            enabled: true,
        }],
    };
    manager.connect_all(&config).await.unwrap();
    let snaps = manager.status_snapshots();
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    assert_eq!(snaps[0].status, ServerStatus::Ready, "{snaps:?}");
    assert!(snaps[0].tool_count > 0, "{snaps:?}");
    // The npm-published server (pre-July TS SDK) does not implement
    // `server/discover`, so the probe is rejected and the fallback
    // settles legacy — exactly the dual-era path this client exists
    // for. Pin only that the connect itself is healthy.
    assert_eq!(
        manager.era_of("everything"),
        Some(era::Era::Legacy),
        "{snaps:?}"
    );
}

#[tokio::test]
async fn disconnect_clears_running_and_failures() {
    let mut manager = McpManager::new();
    attach(&mut manager, "srv", "srv.tool", 0).await;
    manager.disconnect("srv").await;

    assert_eq!(manager.owner_of("srv.tool"), None);
    assert!(manager.all_tools().is_empty());
    let snapshots = manager.status_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(matches!(
        snapshots[0].status,
        super::super::ServerStatus::Connecting
    ));
}

#[tokio::test]
async fn connect_all_isolates_failures() {
    let mut manager = McpManager::new();
    let config = McpConfig {
        servers: vec![http_entry("dead", "http://127.0.0.1:1/mcp")],
    };
    manager.connect_all(&config).await.unwrap();

    let snapshots = manager.status_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(matches!(
        snapshots[0].status,
        super::super::ServerStatus::Failed
    ));
    assert!(snapshots[0].last_error.is_some());
}

#[tokio::test]
async fn connect_one_failure_leaves_failed_snapshot() {
    let mut manager = McpManager::new();
    let entry = http_entry("dead", "http://127.0.0.1:1/mcp");
    assert!(manager.connect_one(&entry).await.is_err());

    let snapshots = manager.status_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(matches!(
        snapshots[0].status,
        super::super::ServerStatus::Failed
    ));
    assert!(snapshots[0].last_error.is_some());
}

#[tokio::test]
async fn connect_all_reconciles_removed_servers() {
    let mut manager = McpManager::new();
    attach(&mut manager, "good", "good.tool", 0).await;

    let config = McpConfig {
        servers: vec![http_entry("dead", "http://127.0.0.1:1/mcp")],
    };
    manager.connect_all(&config).await.unwrap();

    assert_eq!(manager.owner_of("good.tool"), None);
    let snapshots = manager.status_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(matches!(
        snapshots[0].status,
        super::super::ServerStatus::Failed
    ));
}

#[tokio::test]
async fn connect_all_skips_disabled_without_dialing() {
    let mut manager = McpManager::new();
    let mut entry = http_entry("off", "http://127.0.0.1:1/mcp");
    entry.enabled = false;
    let config = McpConfig {
        servers: vec![entry],
    };
    manager.connect_all(&config).await.unwrap();

    let snapshots = manager.status_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(matches!(
        snapshots[0].status,
        super::super::ServerStatus::Disabled
    ));
}

#[tokio::test]
async fn invalid_config_aborts_before_state_changes() {
    let mut manager = McpManager::new();
    let entry = McpServerEntry {
        name: "bad".into(),
        transport: McpTransport::Http(HttpTransport {
            url: "not a url".into(),
            headers: HashMap::new(),
            api_key_env: None,
            timeout_ms: 0,
        }),
        enabled: true,
    };
    assert!(manager.connect_one(&entry).await.is_err());
    assert!(manager.status_snapshots().is_empty());
}

#[test]
fn http_headers_reject_garbage() {
    let mut headers = HashMap::new();
    headers.insert("X-Ok".to_string(), "yes".to_string());
    let entry = McpServerEntry {
        name: "h".into(),
        transport: McpTransport::Http(HttpTransport {
            url: "https://example.com/mcp".into(),
            headers,
            api_key_env: None,
            timeout_ms: 1000,
        }),
        enabled: true,
    };
    let McpTransport::Http(http) = &entry.transport else {
        panic!("expected http");
    };
    assert!(http_config(http, None).is_ok());

    let bad_name = HttpTransport {
        url: "https://example.com/mcp".into(),
        headers: HashMap::from([("not a header".to_string(), "x".to_string())]),
        api_key_env: None,
        timeout_ms: 1000,
    };
    assert!(http_config(&bad_name, None).is_err());

    let bad_value = HttpTransport {
        url: "https://example.com/mcp".into(),
        headers: HashMap::from([("x-ok".to_string(), "bad\nvalue".to_string())]),
        api_key_env: None,
        timeout_ms: 1000,
    };
    assert!(http_config(&bad_value, None).is_err());

    // The resolved credential rides as Authorization: Bearer on the dial.
    let keyed = HttpTransport {
        url: "https://example.com/mcp".into(),
        headers: HashMap::new(),
        api_key_env: None,
        timeout_ms: 1000,
    };
    assert!(http_config(&keyed, Some("sk-test")).is_ok());
    // A key with header-illegal characters is a typed failure, NOT an echo
    // of the secret.
    let err = http_config(&keyed, Some("bad\nkey"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("authorization"), "{err}");
    assert!(!err.contains("bad"), "secret must not leak: {err}");
}
/// Live end-to-end connect against the hosted mem0 MCP server (a legacy
/// server that answers the modern probe with an uncorrelated JSON-RPC
/// error — the regression that added the flip arm in
/// `map_initialize_error`). Ignored by default: needs network access and
/// the `mcp:mem0` credential in the OS keyring. Run explicitly with
/// `cargo test --lib -- --ignored live_mem0`.
#[tokio::test]
#[ignore = "live: real network + keyring (mcp:mem0) required"]
async fn live_mem0_connects_via_flip() {
    use super::super::auth;
    let key = auth::resolve_key("mem0", None).unwrap();
    assert!(key.is_some(), "no mcp:mem0 key in keyring");
    let mut manager = McpManager::new();
    let entry = McpServerEntry {
        name: "mem0".into(),
        transport: McpTransport::Http(HttpTransport {
            url: "https://mcp.mem0.ai/mcp/".into(),
            headers: HashMap::new(),
            api_key_env: None,
            timeout_ms: 10_000,
        }),
        enabled: true,
    };
    manager.connect_one(&entry).await.unwrap();
    let snaps = manager.status_snapshots();
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    assert_eq!(snaps[0].status, ServerStatus::Ready, "{snaps:?}");
    assert!(snaps[0].tool_count > 0, "{snaps:?}");
    assert_eq!(
        manager.era_of("mem0"),
        Some(era::Era::Legacy),
        "mem0 must have settled via the legacy flip"
    );
}
