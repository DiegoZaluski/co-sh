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

#[test]
fn local_header_keeps_inline_tool_format() {
    use std::collections::HashSet;
    // ollama runs on localhost — it may lack reliable native function
    // calling, so it keeps the inline-JSON TOOL_FORMAT as a fallback.
    let connector = Connector::new("ollama").expect("ollama provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("respond with a JSON object"),
        "local providers keep the inline instruction as a fallback"
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

/// Local model servers (ollama/lmstudio/vllm/llamacpp) may lack reliable
/// native function calling — the model emits inline JSON the extractor
/// parses, so the full schemas MUST stay in the header for them.
#[test]
fn local_header_keeps_inline_schemas() {
    use std::collections::HashSet;

    let connector = Connector::new("ollama").expect("ollama provider");
    let mut h = Harness::new(connector, "/tmp", HashSet::new());
    h.format_header_context();
    let header = h.build_chat_context_for_test();
    assert!(
        header.contains("Schema:"),
        "local providers keep the inline schemas for the inline-JSON extractor path"
    );
}
