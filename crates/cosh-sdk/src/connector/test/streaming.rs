//! Tests for `Connector::stream_chat()` and `Connector::stream_chat_with_system()`.
//!
//! Covers: receiving all SSE chunks, system prompt inclusion in the request body,
//! HTTP error propagation before stream start, and stream termination without
//! a `[DONE]` signal.

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
    let stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let chunks: Vec<String> = stream.filter_map(|r| r.ok()).collect().await;
    assert_eq!(chunks.concat(), "Hello world");
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
    let stream = c
        .stream_chat_with_system("user text", "system text")
        .await
        .unwrap();
    let _: Vec<String> = stream.filter_map(|r| r.ok()).collect().await;
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

    let results: Vec<Result<String, _>> = stream.collect().await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap(), "partial");
    assert!(
        results[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("Stream terminated")
    );
}
