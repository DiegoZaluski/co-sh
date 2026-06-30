use super::super::core::Harness;
use serde_json::json;

fn make_harness() -> Harness {
    Harness::new_test()
}

// ── chat() — batch mode ───────────────────────────────────────────

#[tokio::test]
async fn chat_returns_mock_text() {
    let mut h = make_harness().with_mock_chat(Ok("hello world"));

    let result = h.chat("ping").await.unwrap();
    assert_eq!(result, "hello world");
}

#[tokio::test]
async fn chat_returns_mock_error() {
    let mut h = make_harness().with_mock_chat(Err("provider unavailable"));

    let err = h.chat("ping").await.unwrap_err();
    assert!(err.contains("provider unavailable"));
}

#[tokio::test]
async fn chat_extracts_tool_call_into_queue() {
    let mut h = make_harness()
        .with_test_tool(
            "filesystem.read",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        )
        .with_mock_chat(Ok(
            r#"Some text before {"name": "filesystem.read", "arguments": {"path": "/a"}}"#,
        ));

    let result = h.chat("use tool").await.unwrap();

    assert_eq!(
        result, "Some text before ",
        "text before tool call preserved"
    );

    let err = h.dispatch_next().await.unwrap_err();
    assert!(
        err.contains("no server found"),
        "tool queued but no session"
    );
}

#[tokio::test]
async fn chat_extracts_internal_tool_and_handles_immediately() {
    let mut h = make_harness().with_mock_chat(Ok(
        r#"{"name": "expand_namespace", "arguments": {"server": "s1", "namespace": "ns1"}}"#,
    ));

    let result = h.chat("expand").await.unwrap();

    assert!(
        result.is_empty(),
        "internal tool text should be empty: {result:?}"
    );

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no pending tool calls"));
}

#[tokio::test]
async fn chat_extracts_multiple_tool_calls_in_order() {
    let mut h = make_harness()
        .with_test_tool(
            "alpha",
            json!({ "type": "object", "properties": { "i": { "type": "integer" } }, "required": ["i"] }),
        )
        .with_test_tool(
            "beta",
            json!({ "type": "object", "properties": { "i": { "type": "integer" } }, "required": ["i"] }),
        )
        .with_mock_chat(Ok(
            r#"First {"name": "alpha", "arguments": {"i": 1}} then {"name": "beta", "arguments": {"i": 2}}"#,
        ));

    let result = h.chat("do both").await.unwrap();
    assert_eq!(result, "First  then ");
}

// ── stream_chat() — streaming mode ────────────────────────────────

#[tokio::test]
async fn stream_chat_yields_tokens() {
    let mut h = make_harness().with_mock_stream(Ok(vec!["a", "b", "c"]));

    let mut tokens = Vec::new();
    let result = h
        .stream_chat("test", &mut |t: &str| tokens.push(t.to_string()))
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert_eq!(tokens, vec!["a", "b", "c"]);
}

#[tokio::test]
async fn stream_chat_returns_error() {
    let mut h = make_harness().with_mock_stream(Err("stream failed"));

    let mut tokens = Vec::new();
    let err = h
        .stream_chat("test", &mut |t: &str| tokens.push(t.to_string()))
        .await
        .unwrap_err();

    assert!(err.contains("stream failed"));
    assert!(tokens.is_empty(), "no tokens on error");
}

#[tokio::test]
async fn stream_chat_extracts_tool_call() {
    let mut h = make_harness()
        .with_test_tool(
            "fetch",
            json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        )
        .with_mock_stream(Ok(vec![
            "before ",
            r#"{"name": "fetch", "arguments": {"url": "x"}}"#,
            " after",
        ]));

    let mut text_parts = Vec::new();
    let result = h
        .stream_chat("fetch it", &mut |t: &str| text_parts.push(t.to_string()))
        .await;

    assert!(result.is_ok());
    assert_eq!(text_parts, vec!["before ", " after"]);

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found"), "tool was queued");
}

#[tokio::test]
async fn stream_chat_interleaves_text_and_tool_calls() {
    let mut h = make_harness()
        .with_test_tool(
            "search",
            json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
        )
        .with_mock_stream(Ok(vec![
            "Let me check",
            r#"{"name": "search", "arguments": {"q": "rust"}}"#,
            "Here are the results",
        ]));

    let mut texts = Vec::new();
    let _ = h
        .stream_chat("search", &mut |t: &str| texts.push(t.to_string()))
        .await;

    assert_eq!(texts, vec!["Let me check", "Here are the results"]);

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found"), "tool was queued: {err}");
}
