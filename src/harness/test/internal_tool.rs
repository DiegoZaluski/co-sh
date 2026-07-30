use super::super::core::Harness;
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
    };

    let result = h.handle_harness_tool(&tc);
    assert_eq!(result, Some(String::new()));
    assert!(h.stop);
}

#[test]
fn handle_harness_tool_expand_context() {
    let mut h = make_harness();
    let tc = ToolCallData {
        id: String::new(),
        name: "expand_context".into(),
        arguments: json!({"hash": 42, "lv": 2}),
    };

    let result = h.handle_harness_tool(&tc);
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(
        msg.contains("not found"),
        "should report entry not found: {msg}"
    );
}

#[test]
fn handle_harness_tool_force_compress() {
    let mut h = make_harness();
    let tc = ToolCallData {
        id: String::new(),
        name: "force_compress".into(),
        arguments: json!({"checkpoint": 5}),
    };

    let result = h.handle_harness_tool(&tc);
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(
        msg.contains("No fresh context"),
        "should report nothing to compress: {msg}"
    );
}
