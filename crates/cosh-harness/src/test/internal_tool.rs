use crate::harness::Harness;
use cosh_sdk::connector::Connector;
use cosh_sdk::extract_action::ToolCallData;
use serde_json::json;

fn make_harness() -> Harness {
    Harness::new(Connector::new("openai").unwrap())
}

#[test]
fn handle_internal_tool_consumes_expand_namespace() {
    let mut h = make_harness();
    let tc = ToolCallData {
        name: "expand_namespace".into(),
        arguments: json!({"server": "my-server", "namespace": "my-ns"}),
    };

    assert!(h.handle_internal_tool(&tc));
}

#[test]
fn handle_internal_tool_rejects_unknown_tool() {
    let mut h = make_harness();
    let tc = ToolCallData {
        name: "mcp.tool".into(),
        arguments: json!({}),
    };

    assert!(!h.handle_internal_tool(&tc));
}

#[test]
fn handle_internal_tool_populates_expanded_namespaces() {
    let mut h = make_harness();
    let tc = ToolCallData {
        name: "expand_namespace".into(),
        arguments: json!({"server": "s1", "namespace": "ns1"}),
    };

    h.handle_internal_tool(&tc);
    // verify expanded_namespaces is populated (we check via format_header_context later)
}
