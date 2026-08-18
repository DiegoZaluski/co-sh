use super::super::core::{Harness, StreamEvent};
use cosh_sdk::extract_action::NativeToolCall;
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

    let out = h.dispatch_next().await.unwrap();
    assert_eq!(out, "ok:filesystem.read", "tool was queued and dispatched");
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
        .stream_chat("test", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                tokens.push(t);
            }
        })
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
        .stream_chat("test", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                tokens.push(t);
            }
        })
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
        .stream_chat("fetch it", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                text_parts.push(t);
            }
        })
        .await;

    assert!(result.is_ok());
    assert_eq!(text_parts, vec!["before ", " after"]);

    let out = h.dispatch_next().await.unwrap();
    assert_eq!(out, "ok:fetch", "tool was queued and dispatched");
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
        .stream_chat("search", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                texts.push(t);
            }
        })
        .await;

    assert_eq!(texts, vec!["Let me check", "Here are the results"]);

    let out = h.dispatch_next().await.unwrap();
    assert_eq!(out, "ok:search", "tool was queued and dispatched");
}

#[tokio::test]
async fn native_mode_never_parses_text_json() {
    // Default mode (Native, like crush): text tokens are the model's
    // answer. Even a well-formed tool-call envelope in prose must NOT be
    // turned into a queued tool call — the two delivery paths never cross.
    let mut h = make_harness()
        .with_test_tool(
            "fetch",
            json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        )
        .with_tool_call_mode(cosh_sdk::connector::ToolCallMode::Native)
        .with_mock_stream(Ok(vec![
            "before ",
            r#"{"name": "fetch", "arguments": {"url": "x"}}"#,
            " after",
        ]));

    let mut texts = Vec::new();
    let _ = h
        .stream_chat("fetch it", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                texts.push(t);
            }
        })
        .await;

    assert_eq!(
        texts,
        vec![
            "before ",
            r#"{"name": "fetch", "arguments": {"url": "x"}}"#,
            " after",
        ],
        "native mode emits text verbatim — JSON envelope included"
    );
    assert!(!h.has_pending_tools(), "native mode never queues from text");
}

#[tokio::test]
async fn inline_mode_parses_text_json() {
    // Explicit Inline mode: the same text stream IS parsed, and the JSON
    // envelope becomes a queued tool call.
    let mut h = make_harness()
        .with_test_tool(
            "fetch",
            json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        )
        .with_tool_call_mode(cosh_sdk::connector::ToolCallMode::Inline)
        .with_mock_stream(Ok(vec![
            "before ",
            r#"{"name": "fetch", "arguments": {"url": "x"}}"#,
            " after",
        ]));

    let mut texts = Vec::new();
    let _ = h
        .stream_chat("fetch it", &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                texts.push(t);
            }
        })
        .await;

    assert_eq!(texts, vec!["before ", " after"]);
    let out = h.dispatch_next().await.unwrap();
    assert_eq!(out, "ok:fetch", "tool was queued and dispatched");
}

#[tokio::test]
async fn stream_chat_routes_native_tool_call_directly() {
    // A provider-delivered structured tool call (native `tool_calls` /
    // `tool_use` / `functionCall` part) must skip the text parser and queue
    // straight for dispatch, while text around it still streams.
    let mut h = make_harness()
        .with_test_tool(
            "fetch",
            json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        )
        .with_mock_stream(Ok(vec!["before "]))
        .with_mock_native_calls(vec![NativeToolCall {
            id: "call_abc".to_string(),
            name: "fetch".to_string(),
            arguments: r#"{"url": "https://x"}"#.to_string(),
            thought_signature: String::new(),
        }]);

    let mut texts = Vec::new();
    let _ = h
        .stream_chat_with_messages("sys", &[], &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                texts.push(t);
            }
        })
        .await;

    assert_eq!(texts, vec!["before "], "text still streams");

    let out = h.dispatch_next().await.unwrap();
    assert_eq!(out, "ok:fetch", "native tool was queued and dispatched");
}

#[tokio::test]
async fn stream_chat_native_call_invalid_schema_feeds_correction_memory() {
    // A native call that fails schema validation must NOT be queued for
    // dispatch — it is counted and recorded for the correction memory, like
    // a failed inline-JSON call.
    let mut h = make_harness()
        .with_test_tool(
            "fetch",
            json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        )
        .with_mock_stream(Ok(vec![]))
        .with_mock_native_calls(vec![NativeToolCall {
            id: "call_1".to_string(),
            name: "fetch".to_string(),
            arguments: r#"{}"#.to_string(), // missing required "url"
            thought_signature: String::new(),
        }]);

    let mut texts = Vec::new();
    let _ = h
        .stream_chat_with_messages("sys", &[], &mut |e: StreamEvent| {
            if let StreamEvent::Token(t) = e {
                texts.push(t);
            }
        })
        .await;

    assert!(
        texts.iter().any(|t| t.contains("Tool call failure")),
        "failed native call streams the failure warning"
    );
    assert!(!h.has_pending_tools(), "invalid call must not be queued");
    // The failure is counted inside the extractor (`take_tool_failures`)
    // and the raw envelope lands in `last_failed_raw` — both feed the
    // correction memory on the next loop iteration, exactly like a failed
    // inline-JSON call (unit-tested in the extractor).
}

#[tokio::test]
async fn stream_reset_marker_discards_partial_then_restarts() {
    // The SDK emits a reset marker when it retries a mid-stream failure:
    // the harness must forward it to the consumer BEFORE the retried tokens
    // so the partial content of the failed attempt is discarded, never
    // concatenated with the retried response.
    let mut h = make_harness().with_mock_stream_reset().with_mock_stream(Ok(vec![
        "retried-",
        "answer",
    ]));

    let mut events = Vec::new();
    let _ = h
        .stream_chat_with_messages("sys", &[], &mut |e: StreamEvent| events.push(e))
        .await;

    assert_eq!(
        events,
        vec![
            StreamEvent::Reset,
            StreamEvent::Token("retried-".to_string()),
            StreamEvent::Token("answer".to_string()),
        ],
        "the reset marker arrives first, then the retried response"
    );
}
