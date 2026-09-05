use cosh_sdk::connector::{ToolDefinition, ToolFunction};
use cosh_sdk::extract_action::ToolSchema;
use rmcp::model::{CallToolResult, Tool};

/// Expose one MCP tool to the inline-JSON extractor.
pub fn tool_to_schema(tool: &Tool) -> ToolSchema {
    ToolSchema {
        name: tool.name.to_string(),
        input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
    }
}

/// Expose one MCP tool to the native function-calling API. Tools without a
/// description omit the field instead of sending an empty string.
/// `title`, `annotations` (e.g. `destructive_hint`), `output_schema` and
/// `icons` are intentionally unmapped: the native function shape has no
/// fields for them. Wiring safety hints into permission gating is a
/// follow-up, not MVP scope.
pub fn tool_to_definition(tool: &Tool) -> ToolDefinition {
    let mut function = ToolFunction::new(tool.name.to_string());
    if let Some(description) = tool.description.as_deref()
        && !description.is_empty()
    {
        function = function.with_description(description);
    }
    function = function.with_parameters(serde_json::Value::Object((*tool.input_schema).clone()));
    ToolDefinition::new(function)
}

/// Render a tool result as plain text: text blocks joined by newline, falling
/// back to compact structured JSON when the server sent no text at all.
/// Non-text blocks (images, audio, resources) are out of scope for the
/// tools-first MVP and ignored. Callers must check [`is_tool_error`]
/// separately: error results render like successes here by design.
pub fn result_to_text(result: &CallToolResult) -> String {
    let text: Vec<String> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    if !text.is_empty() {
        return text.join("\n");
    }
    match &result.structured_content {
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
        None => String::new(),
    }
}

/// Whether the server reported the tool call itself as failed.
pub const fn is_tool_error(result: &CallToolResult) -> bool {
    matches!(result.is_error, Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
}
