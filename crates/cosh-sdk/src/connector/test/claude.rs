use super::super::{Connector, ToolDefinition, ToolFunction};
use super::common::{ENV_LOCK, EnvGuard, claude_connector, claude_connector_no_key, mock_server};
use tokio_stream::StreamExt;

#[tokio::test]
async fn successful_chat() {
    let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Hello world"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":20}}"#;
    let (port, _body, _raw, handle) = mock_server(body, 200);
    let result = claude_connector(port).chat("hello").await;
    handle.join().unwrap();
    let output = result.unwrap();
    assert_eq!(output.message(), "Hello world");
}

#[tokio::test]
async fn raw_json() {
    let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Hi"}],"stop_reason":"end_turn"}"#;
    let (port, _body, _raw, handle) = mock_server(body, 200);
    let result = claude_connector(port).chat("hello").await;
    handle.join().unwrap();
    let output = result.unwrap();
    assert_eq!(output.message(), "Hi");
    assert_eq!(output.raw(), body);
}

#[tokio::test]
async fn system_prompt_as_top_level_field() {
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let result = claude_connector(port)
        .chat_with_system("user text", "system text")
        .await;
    handle.join().unwrap();
    assert!(result.is_ok());

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["system"], "system text");
    assert_eq!(json["max_tokens"], 4096);
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "user text");
}

#[tokio::test]
async fn params_serialized() {
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let _ = claude_connector(port)
        .with_model("claude-opus-4-8")
        .with_max_tokens(1024)
        .with_temperature(0.7)
        .chat("hello")
        .await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["model"], "claude-opus-4-8");
    assert_eq!(json["max_tokens"], 1024);
    assert!((json["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
}

#[tokio::test]
async fn with_stop_sequences() {
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let _ = claude_connector(port)
        .with_stop(serde_json::json!(["END", "STOP"]))
        .chat("hello")
        .await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["stop_sequences"], serde_json::json!(["END", "STOP"]));
}

#[tokio::test]
async fn tools_serialized() {
    let tool = ToolDefinition::new(
        ToolFunction::new("get_weather")
            .with_description("Get the weather")
            .with_parameters(serde_json::json!({"type":"object"})),
    );
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let _ = claude_connector(port)
        .with_tools(vec![tool])
        .chat("hello")
        .await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["tools"][0]["name"], "get_weather");
    assert_eq!(json["tools"][0]["description"], "Get the weather");
    assert_eq!(
        json["tools"][0]["input_schema"],
        serde_json::json!({"type":"object"})
    );
}

#[tokio::test]
async fn streaming_receives_tokens() {
    let sse = "\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
    let (port, _body, _raw, handle) = mock_server(sse, 200);
    let c = claude_connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let mut tokens = String::new();
    while let Some(chunk) = stream.next().await {
        tokens.push_str(chunk.unwrap().token());
    }
    assert_eq!(tokens, "Hello world");
}

#[tokio::test]
async fn streaming_finish_reason() {
    let sse = "\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
    let (port, _body, _raw, handle) = mock_server(sse, 200);
    let c = claude_connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.token(), "Hello");
    assert_eq!(first.finish_reason(), None);

    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.token(), "");
    assert_eq!(second.finish_reason(), Some("end_turn"));
}

#[tokio::test]
async fn raw_last_frame_with_usage() {
    let last_frame = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#;
    let sse = format!(
        "\
data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"Hi\"}}}}\n\n\
data: {last_frame}\n\n\
data: {{\"type\":\"message_stop\"}}\n\n"
    );
    let (port, _body, _raw, handle) = mock_server(&sse, 200);
    let c = claude_connector(port);
    let mut stream = c.stream_chat("hi").await.unwrap();
    handle.join().unwrap();

    while stream.next().await.is_some() {}
    let raw = stream.raw().await.unwrap();
    assert_eq!(raw, last_frame);
}

#[tokio::test]
async fn http_401() {
    let (port, _body, _raw, handle) = mock_server(
        r#"{"type":"error","error":{"type":"authentication_error","message":"Invalid API key"}}"#,
        401,
    );
    let err = claude_connector(port).chat("hello").await.unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("401"), "status code missing from: {msg}");
}

#[tokio::test]
async fn malformed_json() {
    let (port, _body, _raw, handle) = mock_server("not-json", 200);
    let err = claude_connector(port).chat("hello").await.unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("expected") || err.to_string().contains("invalid"));
}

#[tokio::test]
async fn empty_content() {
    let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"stop_reason":"end_turn"}"#;
    let (port, _body, _raw, handle) = mock_server(body, 200);
    let err = claude_connector(port).chat("hello").await.unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.to_string(), "No content in response");
}

#[tokio::test]
async fn http_500() {
    let (port, _body, _raw, handle) = mock_server("Internal Server Error", 500);
    let err = claude_connector(port).chat("hello").await.unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn network_error() {
    let err = claude_connector(0).chat("hello").await.unwrap_err();
    assert!(
        err.to_string().contains("error")
            || err.to_string().contains("refused")
            || err.to_string().contains("reset"),
        "transport error expected, got: {}",
        err
    );
}

#[tokio::test]
async fn missing_api_key() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("ANTHROPIC_API_KEY");
    let err = Connector::new("claude")
        .unwrap()
        .chat("hello")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("API key not set for provider: claude"),
        "got: {}",
        err
    );
}

#[tokio::test]
async fn api_key_env_fallback() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-from-env");
    let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#;
    let (port, _body, _raw, handle) = mock_server(body, 200);
    let result = claude_connector_no_key(port).chat("hello").await;
    handle.join().unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn model_fallback() {
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let _ = claude_connector(port).chat("hello").await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["model"], "claude-sonnet-4-6");
}

#[tokio::test]
async fn streaming_http_error_propagates() {
    let (port, _body, _raw, handle) = mock_server("Internal Server Error", 500);
    let c = claude_connector(port);
    let result = c.stream_chat("hi").await;
    handle.join().unwrap();
    match result {
        Err(e) => assert!(e.to_string().contains("500")),
        Ok(_) => panic!("expected error, got stream"),
    }
}

#[tokio::test]
async fn user_id_in_metadata() {
    let (port, captured, _raw, handle) = mock_server(
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        200,
    );
    let _ = claude_connector(port)
        .with_user("user-123")
        .chat("hello")
        .await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["metadata"]["user_id"], "user-123");
}
