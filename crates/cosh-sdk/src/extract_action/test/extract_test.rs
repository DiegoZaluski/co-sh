use crate::extract_action::{ExtractAction, Item, StreamAction, ToolSchema};

fn make_extractor() -> ExtractAction {
    let test_tool = ToolSchema {
        name: "expand_namespace".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "server": { "type": "string" },
                "namespace": { "type": "string" }
            },
            "required": ["server", "namespace"]
        }),
    };
    let read_tool = ToolSchema {
        name: "fs.read".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }),
    };
    ExtractAction::new()
        .with_tool(test_tool)
        .with_tool(read_tool)
}

// ---------------------------------------------------------------------------
// extract_batch
// ---------------------------------------------------------------------------

#[test]
fn batch_no_tool_call() {
    let mut ex = make_extractor();
    let result = ex.extract_batch("Hello world");
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(t) => assert_eq!(t, "Hello world"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn batch_simple_tool_call() {
    let mut ex = make_extractor();
    let text = r#"I will call {"name": "expand_namespace", "arguments": {"server": "prod", "namespace": "acme"}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 2);
    match &result.items[0] {
        Item::Text(t) => assert_eq!(t, "I will call "),
        other => panic!("expected Text, got {other:?}"),
    }
    match &result.items[1] {
        Item::ToolCall(tc) => {
            assert_eq!(tc.name, "expand_namespace");
            assert_eq!(
                tc.arguments,
                serde_json::json!({"server": "prod", "namespace": "acme"})
            );
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn batch_multiple_tool_calls() {
    let mut ex = make_extractor();
    let text = r#"first {"name": "fs.read", "arguments": {"path": "/a"}} second {"name": "expand_namespace", "arguments": {"server": "s", "namespace": "n"}} third"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 5);
    match &result.items[0] {
        Item::Text(t) => assert_eq!(t, "first "),
        other => panic!("expected Text, got {other:?}"),
    }
    match &result.items[1] {
        Item::ToolCall(tc) => assert_eq!(tc.name, "fs.read"),
        other => panic!("expected ToolCall, got {other:?}"),
    }
    match &result.items[2] {
        Item::Text(t) => assert_eq!(t, " second "),
        other => panic!("expected Text, got {other:?}"),
    }
    match &result.items[3] {
        Item::ToolCall(tc) => assert_eq!(tc.name, "expand_namespace"),
        other => panic!("expected ToolCall, got {other:?}"),
    }
    match &result.items[4] {
        Item::Text(t) => assert_eq!(t, " third"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_text_before_and_after() {
    let mut ex = make_extractor();
    let text = r#"prefix {"name": "fs.read", "arguments": {"path": "/x"}} suffix"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 3);
    match &result.items[0] {
        Item::Text(t) => assert_eq!(t, "prefix "),
        other => panic!("expected Text, got {other:?}"),
    }
    match &result.items[1] {
        Item::ToolCall(tc) => assert_eq!(tc.name, "fs.read"),
        other => panic!("expected ToolCall, got {other:?}"),
    }
    match &result.items[2] {
        Item::Text(t) => assert_eq!(t, " suffix"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_unknown_tool_name_is_not_captured() {
    let mut ex = make_extractor();
    let text = r#"{"name": "nonexistent", "arguments": {"x": 1}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(_) => {} // not captured, treated as text
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_missing_required_field_is_not_captured() {
    let mut ex = make_extractor();
    let text = r#"{"name": "fs.read", "arguments": {}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(_) => {} // missing "path" required field
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_not_an_object_is_not_captured() {
    let mut ex = make_extractor();
    let text = r#"{"name": "fs.read", "arguments": null}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(_) => {}
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_empty_input() {
    let mut ex = make_extractor();
    let result = ex.extract_batch("");
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(t) => assert!(t.is_empty()),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_no_tools_registered() {
    let mut ex = ExtractAction::new();
    let text = r#"{"name": "fs.read", "arguments": {"path": "/"}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::Text(_) => {}
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn batch_tool_call_with_input_key() {
    let mut ex = make_extractor();
    let text = r#"{"name": "fs.read", "input": {"path": "/a"}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1, "expected just tool: {result:?}");
    match &result.items[0] {
        Item::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
            assert_eq!(tc.arguments, serde_json::json!({"path": "/a"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn batch_tool_call_with_args_key() {
    let mut ex = make_extractor();
    let text = r#"{"name": "fs.read", "args": {"path": "/a"}}"#;
    let result = ex.extract_batch(text);
    assert_eq!(result.items.len(), 1);
    match &result.items[0] {
        Item::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// extract_stream
// ---------------------------------------------------------------------------

#[test]
fn stream_plain_text_passes_through() {
    let mut ex = make_extractor();
    let result = ex.extract_stream("Hello world");
    match result {
        StreamAction::Text(t) => assert_eq!(t, "Hello world"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_multi_token_plain_text() {
    let mut ex = make_extractor();
    let r1 = ex.extract_stream("Hello ");
    match r1 {
        StreamAction::Text(t) => assert_eq!(t, "Hello "),
        other => panic!("expected Text, got {other:?}"),
    }
    let r2 = ex.extract_stream("world");
    match r2 {
        StreamAction::Text(t) => assert_eq!(t, "world"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_complete_tool_call_in_one_token() {
    let mut ex = make_extractor();
    let text = r#"{"name": "fs.read", "arguments": {"path": "/x"}}"#;
    let result = ex.extract_stream(text);
    match result {
        StreamAction::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
            assert_eq!(tc.arguments, serde_json::json!({"path": "/x"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn stream_tool_call_across_multiple_tokens() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(r#"Before {"#);
    match r {
        StreamAction::Text(t) => assert_eq!(t, "Before "),
        other => panic!("expected Text, got {other:?}"),
    }

    let r = ex.extract_stream(r#""name": "fs.read", "arguments": {"path": "/y"}}"#);
    match r {
        StreamAction::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
            assert_eq!(tc.arguments, serde_json::json!({"path": "/y"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn stream_text_before_tool_call_across_tokens() {
    let mut ex = make_extractor();

    let r = ex.extract_stream("I will call ");
    match r {
        StreamAction::Text(t) => assert_eq!(t, "I will call "),
        other => panic!("expected Text, got {other:?}"),
    }

    let r = ex.extract_stream(r#"{"name"#);
    match r {
        StreamAction::Pending => {}
        other => panic!("expected Pending, got {other:?}"),
    }

    let r = ex.extract_stream(r#"": "fs.read", "arguments": {"path": "/z"}}"#);
    match r {
        StreamAction::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn stream_early_exit_on_unknown_key() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(r#"I think {"#);
    match r {
        StreamAction::Text(t) => assert_eq!(t, "I think "),
        other => panic!("expected Text, got {other:?}"),
    }

    let r = ex.extract_stream(r#""animal": "dog"}"#);
    match r {
        StreamAction::Text(t) => {
            assert!(
                t.contains("Tool call failure"),
                "should flush warning on unknown key: {t:?}"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_pending_state() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(r#"{"name""#);
    match r {
        StreamAction::Pending => {}
        other => panic!("expected Pending, got {other:?}"),
    }
}

#[test]
fn stream_invalid_json_is_flushed_as_text() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(r#"Here: {"name": "fs.read", "bad"#);
    match r {
        StreamAction::Text(t) => assert_eq!(t, "Here: "),
        other => panic!("expected Text, got {other:?}"),
    }

    let r = ex.extract_stream(r#"": {"path": "/"}}"#);
    match r {
        StreamAction::Text(t) => {
            assert!(
                t.contains("Tool call failure"),
                "should flush warning instead of raw JSON: {t:?}"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_no_tool_schema_registered() {
    let mut ex = ExtractAction::new();
    let text = r#"{"name": "fs.read", "arguments": {"path": "/"}}"#;
    let result = ex.extract_stream(text);
    match result {
        StreamAction::Text(t) => {
            assert!(
                t.contains("Tool call failure"),
                "should warn instead of showing raw JSON: {t:?}"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_text_and_tool_in_same_token_no_prefix() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(r#"{"name": "fs.read", "arguments": {"path": "/a"}}"#);
    match r {
        StreamAction::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
            assert_eq!(tc.arguments, serde_json::json!({"path": "/a"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn stream_key_starting_same_as_known_but_different() {
    let mut ex = make_extractor();

    let r = ex.extract_stream("data: ");
    match r {
        StreamAction::Text(t) => assert_eq!(t, "data: "),
        other => panic!("expected Text, got {other:?}"),
    }

    let r = ex.extract_stream(r#"{"nameX"#);
    match r {
        StreamAction::Pending => {}
        other => panic!("expected Pending, got {other:?}"),
    }

    let r = ex.extract_stream(r#"": 1}"#);
    match r {
        StreamAction::Text(t) => {
            assert!(t.contains("Tool call failure"), "should warn: {t:?}");
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn stream_nested_objects_do_not_confuse_depth() {
    let mut ex = make_extractor();

    let r = ex.extract_stream(
        r#"{"name": "fs.read", "arguments": {"path": "/a", "options": {"recursive": true}}}"#,
    );
    match r {
        StreamAction::ToolCall(tc) => {
            assert_eq!(tc.name, "fs.read");
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn batch_ask_questions_tool_call() {
    let ask_schema = ToolSchema {
        name: "ask_questions".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "question": { "type": "string" },
                            "type": { "type": "string", "enum": ["Text", "SingleChoice", "MultiChoice", "YesNo"] },
                            "purpose": { "type": "string" },
                            "options": { "type": "array", "items": { "type": "string" } },
                            "required": { "type": "boolean" }
                        },
                        "required": ["id", "question", "type"]
                    }
                }
            },
            "required": ["questions"]
        }),
    };

    let mut ex = ExtractAction::new().with_tool(ask_schema);

    // Standard envelope
    let text = r#"Some thoughts {"name": "ask_questions", "arguments": {"questions": [{"id": "lang", "question": "What language?", "type": "SingleChoice", "required": true, "options": ["Python", "Rust"]}]}} trailing"#;

    let result = ex.extract_batch(text);

    let tool_items: Vec<_> = result
        .items
        .iter()
        .filter_map(|i| {
            if let Item::ToolCall(tc) = i {
                Some(tc)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !tool_items.is_empty(),
        "Expected at least one tool call, got: {result:?}"
    );
    assert_eq!(tool_items[0].name, "ask_questions");
    assert!(tool_items[0].arguments.get("questions").is_some());
}

#[test]
fn batch_ask_questions_bare_args() {
    let ask_schema = ToolSchema {
        name: "ask_questions".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "question": { "type": "string" },
                            "type": { "type": "string" }
                        },
                        "required": ["id", "question", "type"]
                    }
                }
            },
            "required": ["questions"]
        }),
    };

    let mut ex = ExtractAction::new().with_tool(ask_schema);

    // Bare arguments (no envelope) — triggers the "bare-arguments fallback"
    let text = r#"{"questions": [{"id": "lang", "question": "What?", "type": "Text"}]}"#;
    let result = ex.extract_batch(text);

    let tool_items: Vec<_> = result
        .items
        .iter()
        .filter_map(|i| {
            if let Item::ToolCall(tc) = i {
                Some(tc)
            } else {
                None
            }
        })
        .collect();

    // With the bare-arguments fallback, this should work
    assert!(
        !tool_items.is_empty(),
        "Expected at least one tool call, got: {result:?}"
    );
    assert_eq!(tool_items[0].name, "ask_questions");
}
