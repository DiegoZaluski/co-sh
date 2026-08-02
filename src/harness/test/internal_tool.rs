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

