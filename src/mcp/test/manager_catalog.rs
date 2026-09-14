//! Resource and prompt scopes ([`super::super::manager::McpManager`]).
//!
//! Covers capability-gated listing at connect time, resource reads, prompt
//! fetching, template-based routing, and duplicate resolution.

use super::super::error::McpError;
use super::super::manager::McpManager;
use super::manager_support::{CatalogServer, LegacyOnlyServer, attach_server};

#[tokio::test]
async fn resources_and_prompts_listed_when_capable() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "cat",
        CatalogServer {
            tool: "cat.tool".into(),
        },
    )
    .await;

    let resources = manager.all_resources();
    assert_eq!(resources.len(), 2, "{resources:?}");
    assert_eq!(resources[0].uri, "file:///notes.txt");
    assert!(!resources[0].is_template);
    assert_eq!(resources[0].mime_type.as_deref(), Some("text/plain"));
    assert_eq!(resources[1].uri, "file:///docs/{id}"); // template, raw RFC 6570
    assert!(resources[1].is_template);

    let prompts = manager.all_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_eq!(prompts[0].name, "greet");
    assert_eq!(prompts[0].arguments.len(), 1);
    assert!(prompts[0].arguments[0].required);

    assert_eq!(manager.resource_owner_of("file:///notes.txt"), Some("cat"));
    // Expanded URIs route via template match.
    assert_eq!(manager.resource_owner_of("file:///docs/42"), Some("cat"));
    assert_eq!(manager.resource_owner_of("file:///docs/{id}"), Some("cat"));
    assert_eq!(manager.resource_owner_of("file:///other.txt"), None);
    assert_eq!(manager.prompt_owner_of("greet"), Some("cat"));
}

#[tokio::test]
async fn read_resource_roundtrip_and_unknown() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "cat",
        CatalogServer {
            tool: "cat.tool".into(),
        },
    )
    .await;

    let read = manager.read_resource("file:///notes.txt").await.unwrap();
    assert_eq!(crate::mcp::bridge::resource_to_text(&read), "hello notes");

    let missing = manager
        .read_resource("file:///absent.txt")
        .await
        .unwrap_err();
    assert!(
        matches!(missing, McpError::UnknownResource(_)),
        "{missing:?}"
    );
}

#[tokio::test]
async fn get_prompt_roundtrip_and_unknown() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "cat",
        CatalogServer {
            tool: "cat.tool".into(),
        },
    )
    .await;

    let args = serde_json::from_str::<serde_json::Map<_, _>>(r#"{"who": "Ada"}"#).unwrap();
    let result = manager.get_prompt("greet", args).await.unwrap();
    assert_eq!(
        crate::mcp::bridge::prompt_to_text(&result),
        "user: Say hello to Ada"
    );

    let missing = manager
        .get_prompt("nope", Default::default())
        .await
        .unwrap_err();
    assert!(matches!(missing, McpError::UnknownPrompt(_)), "{missing:?}");
}

#[tokio::test]
async fn duplicate_resource_uris_resolve_in_config_order() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "first",
        CatalogServer {
            tool: "dup.tool".into(),
        },
    )
    .await;
    attach_server(
        &mut manager,
        "second",
        CatalogServer {
            tool: "dup.tool".into(),
        },
    )
    .await;
    // Both advertise `file:///notes.txt`; config order wins.
    assert_eq!(
        manager.resource_owner_of("file:///notes.txt"),
        Some("first")
    );
    assert_eq!(manager.prompt_owner_of("greet"), Some("first"));
    assert_eq!(manager.owner_of("dup.tool"), Some("first"));
}

#[tokio::test]
async fn capability_less_server_skips_resource_and_prompt_calls() {
    let mut manager = McpManager::new();
    attach_server(
        &mut manager,
        "bare",
        LegacyOnlyServer {
            tool: "bare.tool".into(),
        },
    )
    .await;

    // The doubles never got asked: an uncalled resources/list on a
    // server without the capability would be a protocol violation.
    assert!(manager.all_resources().is_empty());
    assert!(manager.all_prompts().is_empty());
    let resource_err = manager.read_resource("file:///x").await.unwrap_err();
    assert!(
        matches!(resource_err, McpError::UnknownResource(_)),
        "{resource_err:?}"
    );
    let prompt_err = manager
        .get_prompt("p", Default::default())
        .await
        .unwrap_err();
    assert!(
        matches!(prompt_err, McpError::UnknownPrompt(_)),
        "{prompt_err:?}"
    );
}
