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
