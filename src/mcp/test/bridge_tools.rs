//! Tool conversion and result rendering ([`super::super::bridge`]).
//!
//! Covers exposing MCP tools to the extractor / function-calling API and
//! rendering tool results as plain text.

use std::sync::Arc;

use rmcp::model::{CallToolResult, Tool};

use super::super::bridge::{is_tool_error, result_to_text, tool_to_definition, tool_to_schema};

fn test_tool() -> Tool {
    let mut tool = Tool::default();
    tool.name = "echo.tool".into();
    tool.description = Some("Echoes input".into());
    tool.input_schema = Arc::new(
        serde_json::json!({"type": "object", "properties": {}})
            .as_object()
            .unwrap()
            .clone(),
    );
    tool
}

#[test]
fn schema_and_definition_carry_name_and_shape() {
    let tool = test_tool();
    let schema = tool_to_schema(&tool);
    assert_eq!(schema.name, "echo.tool");

    let definition = tool_to_definition(&tool);
    let json = serde_json::to_value(&definition).unwrap();
    let text = json.to_string();
    assert!(text.contains("echo.tool"));
}

#[test]
fn result_prefers_text_blocks() {
    let result = CallToolResult::success(vec![
        rmcp::model::ContentBlock::text("one"),
        rmcp::model::ContentBlock::text("two"),
    ]);
    assert_eq!(result_to_text(&result), "one\ntwo");
}

#[test]
fn result_falls_back_to_structured_content() {
    let mut result = CallToolResult::success(vec![]);
    result.structured_content = Some(serde_json::json!({"ok": true}));
    assert_eq!(result_to_text(&result), r#"{"ok":true}"#);
}

#[test]
fn empty_result_renders_empty() {
    let result = CallToolResult::success(vec![]);
    assert_eq!(result_to_text(&result), "");
    assert!(!is_tool_error(&result));
}

#[test]
fn error_flag_is_visible_to_callers() {
    let mut result = CallToolResult::success(vec![rmcp::model::ContentBlock::text("nope")]);
    result.is_error = Some(true);
    assert!(is_tool_error(&result));
    assert_eq!(result_to_text(&result), "nope");
}

#[test]
fn unset_error_flag_counts_as_success() {
    let mut result = CallToolResult::success(vec![]);
    assert!(!is_tool_error(&result));
    result.is_error = Some(false);
    assert!(!is_tool_error(&result));
}

#[test]
fn image_only_result_renders_empty() {
    let result =
        CallToolResult::success(vec![rmcp::model::ContentBlock::image("aGk=", "image/png")]);
    assert_eq!(result_to_text(&result), "");
}

#[test]
fn mixed_text_and_structured_prefers_text() {
    let mut result = CallToolResult::success(vec![rmcp::model::ContentBlock::text("hi")]);
    result.structured_content = Some(serde_json::json!({"ignored": true}));
    assert_eq!(result_to_text(&result), "hi");
}

#[test]
fn definition_omits_missing_description() {
    let mut tool = test_tool();
    tool.description = None;
    let json = serde_json::to_value(tool_to_definition(&tool)).unwrap();
    assert_eq!(json["function"]["name"], "echo.tool");
    assert!(json["function"].get("description").is_none());
}
