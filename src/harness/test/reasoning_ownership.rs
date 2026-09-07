//! Reasoning/signature ownership (CM-04): Claude extended-thinking blocks
//! and Gemini thought signatures belong to the tool call they preceded. They
//! must replay verbatim on the next request, survive masking and restore,
//! and tolerate contexts persisted before the fields existed.

use super::super::context::ContextManager;
use crate::harness::context::ContextItem;
use cosh_sdk::connector::{ChatMessage, ClaudeThinkingBlock};

fn reasoning() -> Vec<ClaudeThinkingBlock> {
    vec![ClaudeThinkingBlock {
        thinking: "need to inspect the tree before answering".into(),
        signature: "sig-abc".into(),
    }]
}

fn turn_with_reasoning() -> ContextManager {
    let mut manager = ContextManager::new(20_000);
    manager.add_user("explore the tree");
    manager.add_tool_call_with_thinking(
        "call-1",
        "bash",
        "{\"cmd\":\"ls\"}",
        "gemini-thought",
        reasoning(),
    );
    manager.add_tool_result("call-1", "file one");
    manager
}

fn assert_same_reasoning(actual: Option<&[ClaudeThinkingBlock]>) {
    let blocks = actual.expect("thinking blocks present");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].thinking, reasoning()[0].thinking);
    assert_eq!(blocks[0].signature, reasoning()[0].signature);
}

fn tool_call_message(messages: &[ChatMessage]) -> &ChatMessage {
    messages
        .iter()
        .find(|m| m.role == "assistant" && m.tool_calls.is_some())
        .expect("tool call message present")
}

#[test]
fn tool_turn_replays_thinking_blocks_and_signature_verbatim() {
    let manager = turn_with_reasoning();
    let messages = manager.build_messages("");
    let call = tool_call_message(&messages);
    let tool_calls = call.tool_calls.as_ref().unwrap();
    assert_eq!(
        tool_calls[0].thought_signature.as_deref(),
        Some("gemini-thought")
    );
    assert_same_reasoning(call.thinking_blocks.as_deref());
}

#[test]
fn save_restore_roundtrip_preserves_reasoning() {
    let state = turn_with_reasoning().save_state();
    let mut restored = ContextManager::new(20_000);
    restored.restore_state(&state);
    let messages = restored.build_messages("");
    let call = tool_call_message(&messages);
    assert_same_reasoning(call.thinking_blocks.as_deref());
    assert_eq!(
        call.tool_calls.as_ref().unwrap()[0]
            .thought_signature
            .as_deref(),
        Some("gemini-thought")
    );
}

#[test]
fn masked_tool_result_keeps_the_call_reasoning_intact() {
    let mut manager = turn_with_reasoning();
    let result_id = *match manager.items_snapshot().last().unwrap() {
        ContextItem::ToolResult { id, .. } => id,
        other => panic!("expected tool result tail, got {other:?}"),
    };
    let mut state = manager.save_state();
    state.masked.insert(result_id);
    manager.restore_state(&state);

    let messages = manager.build_messages("");
    assert_same_reasoning(tool_call_message(&messages).thinking_blocks.as_deref());
    let result = messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-1"))
        .unwrap();
    assert!(result.content.as_deref().unwrap_or("").contains("masked"));
}

#[test]
fn legacy_context_without_reasoning_fields_loads_with_empty_defaults() {
    // Contexts persisted before the reasoning fields existed must still
    // resume: a failed projection replay silently drops the whole session.
    let legacy = r#"{
        "items": [
            {"User": {"id": 1, "original": "explore"}},
            {"ToolCall": {"id": 2, "call_id": "call-1",
             "name": "bash", "arguments": "{\"cmd\":\"ls\"}"}},
            {"ToolResult": {"id": 3, "call_id": "call-1",
             "content": "file one", "useless": false}}
        ],
        "next_id": 4, "max_tokens": 20000,
        "overflow_model": null, "split": null, "visible_from": null,
        "hidden": []
    }"#;
    let state: super::super::context::ContextManagerState =
        serde_json::from_str(legacy).expect("legacy context must deserialize");
    assert_eq!(state.items.len(), 3);
    let mut manager = ContextManager::new(20_000);
    manager.restore_state(&state);
    let legacy_view = manager.build_messages("");
    let call = tool_call_message(&legacy_view);
    assert!(
        call.thinking_blocks
            .as_ref()
            .is_none_or(|blocks| blocks.is_empty())
    );
    assert!(
        call.tool_calls.as_ref().unwrap()[0]
            .thought_signature
            .as_deref()
            .is_none_or(str::is_empty)
    );
}
