use super::super::core::Harness;
use cosh_sdk::connector::Connector;
use cosh_sdk::extract_action::ToolCallData;
use serde_json::json;

fn make_harness() -> Harness {
    Harness::new_test()
}

#[test]
fn handle_harness_tool_rejects_unknown_tool() {
    let mut h = make_harness();
    let tc = ToolCallData {
        id: String::new(),
        name: "mcp.tool".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    };

    assert!(h.handle_harness_tool(&tc).is_none());
}

#[test]
fn handle_harness_tool_consumes_stop_agent_loop() {
    let mut h = make_harness();
    let tc = ToolCallData {
        id: String::new(),
        name: "stop_agent_loop".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    };

    let result = h.handle_harness_tool(&tc);
    assert_eq!(result, Some(String::new()));
    assert!(h.stop);
}

/// `mask_tool_result` is a no-args harness tool: the harness masks the
/// newest tool result in the context manager and reports the source item.
#[test]
fn handle_harness_tool_masks_the_newest_tool_result() {
    let mut h = make_harness();
    h.context_manager.add_user("task");
    h.context_manager.add_tool_call("c1", "fs_read", "{}");
    h.context_manager.add_tool_result("c1", "huge payload");
    h.context_manager.add_assistant("reacted", false);

    let tc = ToolCallData {
        id: String::new(),
        name: "mask_tool_result".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    };
    let result = h.handle_harness_tool(&tc).expect("consumed");
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("json result");
    assert_eq!(parsed["masked"], json!(true));
    assert_eq!(parsed["source_item"], json!(3));
    assert!(h.context_manager.save_state().masked.contains(&3));

    // Nothing left to mask: a typed miss, not an error.
    let result = h.handle_harness_tool(&tc).expect("consumed");
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("json result");
    assert_eq!(parsed["masked"], json!(false));
}

/// A `mask_tool_result` call batched with a real tool call (the encouraged
/// usage) is routed MID-STREAM, before the real call is dispatched. The
/// harness-tool chain must therefore never consume the stashed Claude
/// thinking blocks: those belong to the turn's REAL tool_use, which Anthropic
/// validates verbatim on the follow-up request.
#[test]
fn mask_tool_call_never_consumes_the_thinking_block_stash() {
    use crate::harness::context::ContextItem;
    use cosh_sdk::connector::ClaudeThinkingBlock;
    let mut h = make_harness();
    h.context_manager.add_user("task");
    h.context_manager.add_tool_call("c1", "fs_read", "{}");
    h.context_manager.add_tool_result("c1", "huge payload");
    h.stash_thinking_blocks_for_test(vec![ClaudeThinkingBlock {
        thinking: "thought".into(),
        signature: "sig".into(),
    }]);

    h.route_tool_call(ToolCallData {
        id: "m1".into(),
        name: "mask_tool_result".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    });

    let mask_item = h
        .context_manager
        .items_snapshot()
        .into_iter()
        .find(
            |item| matches!(item, ContextItem::ToolCall { name, .. } if name == "mask_tool_result"),
        )
        .expect("mask call recorded");
    assert!(
        match &mask_item {
            ContextItem::ToolCall {
                thinking_blocks, ..
            } => thinking_blocks.is_empty(),
            _ => false,
        },
        "the harness-tool chain carries no thinking blocks"
    );
    assert_eq!(
        h.pending_thinking_blocks_count_for_test(),
        1,
        "stash untouched by the mask call"
    );
}

/// All cloud providers officially recommend native function calling and warn
/// that inline-JSON instructions in the prompt conflict with it (Gemini 3.x
/// obeys the legacy inline-JSON `TOOL_FORMAT` literally and emits tool calls
/// as raw TEXT even with toolConfig AUTO — the API then rejects the turn with
/// MALFORMED_FUNCTION_CALL and stray JSON/reasoning fragments leak to the
/// user). Cloud providers get the NATIVE instruction; only local model
/// servers (ollama/lmstudio/vllm/llamacpp) keep the inline-JSON fallback.
#[test]
fn cloud_header_uses_native_tool_format() {
    use std::collections::HashSet;
    let connector = Connector::new("gemini").expect("gemini provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("NATIVE function calling mechanism"),
        "gemini must be instructed to use native function calling"
    );
    assert!(
        !header.contains("respond with a JSON object"),
        "gemini must NOT be instructed to emit inline JSON tool calls"
    );

    let connector = Connector::new("openai").expect("openai provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("NATIVE function calling mechanism"),
        "openai cloud must also be instructed to use native function calling"
    );

    let connector = Connector::new("claude").expect("claude provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("NATIVE function calling mechanism"),
        "claude cloud must also be instructed to use native function calling"
    );
}

/// The tool-call MODE decides the instruction — not the provider's
/// locality. Default (Native, like crush): every provider, local included,
/// is told to use the platform's structured function calling.
#[test]
fn local_header_defaults_to_native_tool_format() {
    use std::collections::HashSet;

    use super::super::core::Harness;
    let connector = Connector::new("ollama").expect("ollama provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("NATIVE function calling mechanism"),
        "default mode is Native even for local providers"
    );
    assert!(
        !header.contains("respond with a JSON object"),
        "no inline instruction in Native mode"
    );
}

/// Switching the mode to Inline restores the inline-JSON instruction for
/// ANY provider — the user opts into the text-parsed path explicitly.
#[test]
fn inline_mode_keeps_inline_tool_format() {
    use std::collections::HashSet;

    use super::super::core::Harness;
    use cosh_sdk::connector::ToolCallMode;
    let connector = Connector::new("openai")
        .expect("openai provider")
        .with_tool_call_mode(ToolCallMode::Inline);
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("respond with a JSON object"),
        "Inline mode keeps the inline instruction"
    );
}

/// Cloud providers hold the tool schemas in their native function-calling
/// mechanism (the request's `tools` array) — re-sending them inline in the
/// system prompt duplicates every schema (~4.3k tokens of the measured
/// header) on every request. The header keeps names + descriptions as prose
/// so the model can reason over the tool list, but the `Schema: {...}` block
/// is omitted for non-local providers.
#[test]
fn cloud_header_omits_inline_schemas() {
    use std::collections::HashSet;

    let connector = Connector::new("openai").expect("openai provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        !header.contains("Schema:"),
        "cloud header must not duplicate the native tool schemas inline"
    );
    // Names + descriptions stay as prose guidance.
    assert!(
        header.contains("bash_run"),
        "tool names must remain in the header"
    );
    assert!(
        header.contains("fs_read"),
        "tool names must remain in the header"
    );
}

/// Ask mode uses the filtered writer — the same schema omission must apply
/// to it for cloud providers.
#[test]
fn cloud_ask_header_omits_inline_schemas() {
    use std::collections::HashSet;

    use super::super::core::Mode;
    let connector = Connector::new("openai").expect("openai provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new()).with_mode(Mode::Ask);
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        !header.contains("Schema:"),
        "cloud Ask header must also omit the inline schemas"
    );
    assert!(header.contains("fs_read"));
}

/// The schemas stay inline in the header ONLY in Inline mode (the model
/// must see them to write valid JSON). In Native mode they live exclusively
/// in the request's `tools` array — for local providers too.
#[test]
fn header_schemas_follow_tool_call_mode() {
    use std::collections::HashSet;

    use cosh_sdk::connector::ToolCallMode;

    let connector = Connector::new("ollama").expect("ollama provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        !header.contains("Schema:"),
        "Native mode (default) omits inline schemas even for local providers"
    );

    let connector = Connector::new("ollama")
        .expect("ollama provider")
        .with_tool_call_mode(ToolCallMode::Inline);
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("Schema:"),
        "Inline mode keeps the inline schemas for the extractor path"
    );
}
