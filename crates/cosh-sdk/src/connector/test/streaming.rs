//! Tests for `Connector::stream_chat()` and `Connector::stream_chat_with_system()`.
//!
//! Covers: receiving all SSE chunks, system prompt inclusion in the request body,
//! HTTP error propagation before stream start, and stream termination without
//! a `[DONE]` signal.
use super::super::StreamChunk;
use super::common::{connector, mock_server};
use tokio_stream::StreamExt;

/// Ensures all SSE data chunks are concatenated correctly.
#[tokio::test]
async fn receives_all_chunks() {
    let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n\
data: [DONE]\n\n";
    let (port, _body, _raw, handle) = mock_server(sse, 200);
    let c = connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let mut tokens = String::new();
    while let Some(chunk) = stream.next().await {
        tokens.push_str(chunk.unwrap().token());
    }
    assert_eq!(tokens, "Hello world");

    let raw = stream.raw().await.unwrap();
    assert!(
        raw.contains("world"),
        "last frame should contain 'world', got: {raw}"
    );
}

/// Ensures `.finish_reason()` returns `Some("stop")` on the last content chunk.
#[tokio::test]
async fn finish_reason_on_last_chunk() {
    let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
    let (port, _body, _raw, handle) = mock_server(sse, 200);
    let c = connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.token(), "Hello");
    assert_eq!(first.finish_reason(), None);

    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.token(), "");
    assert_eq!(second.finish_reason(), Some("stop"));
}

/// Ensures `.raw()` on a finished stream returns the last SSE frame with metadata.
#[tokio::test]
async fn raw_last_frame_with_usage() {
    let last_frame = r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
    let sse = format!(
        "\
data: {{\"choices\":[{{\"delta\":{{\"content\":\"Hello\"}}}}]}}\n\n\
data: {last_frame}\n\n\
data: [DONE]\n\n"
    );
    let (port, _body, _raw, handle) = mock_server(&sse, 200);
    let c = connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    while stream.next().await.is_some() {}
    let raw = stream.raw().await.unwrap();
    assert_eq!(raw, last_frame);
}

/// Ensures the system prompt is included in the request body when using
/// `stream_chat_with_system()`.
#[tokio::test]
async fn with_system_prompt_in_body() {
    let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
data: [DONE]\n\n";
    let (port, captured, _raw, handle) = mock_server(sse, 200);
    let c = connector(port);
    let mut stream = c
        .stream_chat_with_system("user text", "system text")
        .await
        .unwrap();
    while stream.next().await.is_some() {}
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "system text");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "user text");
}

/// Ensures an HTTP error on stream start propagates as an `Err` (not a stream).
#[tokio::test]
async fn http_error_propagates() {
    let (port, _body, _raw, handle) = mock_server("Internal Server Error", 500);
    let c = connector(port);
    let result = c.stream_chat("hi").await;
    handle.join().unwrap();
    match result {
        Err(e) => assert!(e.to_string().contains("500")),
        Ok(_) => panic!("expected error, got stream"),
    }
}

/// Ensures a stream that ends without a `[DONE]` signal produces an error.
#[tokio::test]
async fn terminated_without_done() {
    let (port, _body, _raw, handle) = mock_server(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        200,
    );
    let c = connector(port);
    let stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let results: Vec<Result<StreamChunk, _>> = stream.collect().await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap().token(), "partial");
    assert!(
        results[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("Stream terminated")
    );
}

// The LLM summarizer must NEVER see the agent loop's tool definitions: a
// connector with tools configured that calls `stream_chat_with_system_no_tools`
// sends a request WITHOUT the `tools`/`tool_choice` keys — so the model
// answers with prose instead of a tool call.
#[tokio::test]
async fn stream_with_system_no_tools_omits_tools_from_request() {
    use super::super::ToolDefinition;
    use super::super::ToolFunction;

    let tool = ToolDefinition::new(
        ToolFunction::new("read_file")
            .with_description("Read a file")
            .with_parameters(serde_json::json!({"type":"object"})),
    );
    let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"summary\"}}]}\n\n\
data: [DONE]\n\n";
    let (port, captured, _raw, handle) = mock_server(sse, 200);
    let c = connector(port)
        .with_tools(vec![tool])
        .with_tool_choice(serde_json::json!("auto"));
    let mut stream = c
        .stream_chat_with_system_no_tools("summarize", "You are a summarizer")
        .await
        .unwrap();
    handle.join().unwrap();

    // The request body must carry the system prompt but NO tools.
    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "You are a summarizer");
    assert!(
        json.get("tools").is_none(),
        "the summarizer request must not expose tool schemas: {json}"
    );
    assert!(json.get("tool_choice").is_none());

    // The stream still delivers the model's prose.
    let mut tokens = String::new();
    while let Some(chunk) = stream.next().await {
        tokens.push_str(chunk.unwrap().token());
    }
    assert_eq!(tokens, "summary");
}
