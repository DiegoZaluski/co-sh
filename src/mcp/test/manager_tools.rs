//! Tool routing and calls ([`super::super::manager::McpManager`]).
//!
//! Covers owner resolution in config order, typed unknown-tool errors,
//! timeouts, server-side error propagation, duplicate names, and reconnects.

use super::super::error::McpError;
use super::super::manager::McpManager;
use super::manager_support::{attach, attach_failing};

#[tokio::test]
async fn routes_calls_to_owning_server() {
    let mut manager = McpManager::new();
    attach(&mut manager, "a", "alpha.tool", 0).await;
    attach(&mut manager, "b", "beta.tool", 0).await;

    assert_eq!(manager.owner_of("alpha.tool"), Some("a"));
    assert_eq!(manager.owner_of("beta.tool"), Some("b"));
    assert_eq!(manager.owner_of("missing.tool"), None);
    assert_eq!(manager.all_tools().len(), 2);

    let result = manager
        .call_tool("alpha.tool", serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(super::super::result_to_text(&result), "ok");
}

#[tokio::test]
async fn unknown_tool_is_typed() {
    let mut manager = McpManager::new();
    attach(&mut manager, "a", "alpha.tool", 0).await;

    let err = manager
        .call_tool("ghost.tool", serde_json::Map::new())
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::UnknownTool(_)));
}

#[tokio::test]
async fn slow_server_times_out() {
    let mut manager = McpManager::new();
    attach(&mut manager, "slow", "slow.tool", 5000).await;

    let err = manager
        .call_tool("slow.tool", serde_json::Map::new())
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Timeout(name, ms) if name == "slow" && ms == 200));
}

#[tokio::test]
async fn error_flag_propagates_as_call_failure() {
    let mut manager = McpManager::new();
    attach_failing(&mut manager, "flaky", "flaky.tool", 0, true).await;

    let err = manager
        .call_tool("flaky.tool", serde_json::Map::new())
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Call(tool, text) if tool == "flaky.tool" && text == "nope"));
}

#[tokio::test]
async fn duplicate_tool_names_resolve_in_config_order() {
    let mut manager = McpManager::new();
    attach(&mut manager, "first", "dup.tool", 0).await;
    attach(&mut manager, "second", "dup.tool", 0).await;

    assert_eq!(manager.owner_of("dup.tool"), Some("first"));
}

#[tokio::test]
async fn reconnect_replaces_stale_tools() {
    let mut manager = McpManager::new();
    attach(&mut manager, "srv", "old.tool", 0).await;
    assert_eq!(manager.owner_of("old.tool"), Some("srv"));

    attach(&mut manager, "srv", "new.tool", 0).await;
    assert_eq!(manager.owner_of("old.tool"), None);
    assert_eq!(manager.owner_of("new.tool"), Some("srv"));
}
