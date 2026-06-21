use super::super::Connector;
use super::common::{ENV_LOCK, EnvGuard, claude_connector, connector, mock_server};

// OpenAI-compatible

#[tokio::test]
async fn missing_api_key() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let err = Connector::new("openai")
        .unwrap()
        .list_models()
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("API key not set for provider: openai"),
        "got: {}",
        err,
    );
}

#[tokio::test]
async fn openai_success() {
    let raw_body = r#"{"object":"list","data":[{"id":"gpt-4o","object":"model"},{"id":"gpt-4","object":"model"}]}"#;
    let (port, _body, _raw, handle) = mock_server(raw_body, 200);
    let result = connector(port).list_models().await;
    handle.join().unwrap();
    let out = result.unwrap();

    assert_eq!(out.raw(), raw_body);

    let ids: Vec<&str> = out.models().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-4o", "gpt-4"]);
}

#[tokio::test]
async fn openai_http_401() {
    let (port, _body, _raw, handle) =
        mock_server(r#"{"error":{"message":"Incorrect API key"}}"#, 401);
    let err = connector(port).list_models().await.unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("401"), "status code missing from: {msg}");
    assert!(
        msg.contains("Incorrect API key"),
        "detail missing from: {msg}"
    );
}

#[tokio::test]
async fn openai_malformed_json() {
    let (port, _body, _raw, handle) = mock_server("not-json-at-all", 200);
    let err = connector(port).list_models().await.unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("expected") || msg.contains("invalid"),
        "deserialization error expected, got: {msg}",
    );
}

#[tokio::test]
async fn openai_network_error() {
    let err = Connector::new("openai")
        .unwrap()
        .with_base_url("http://127.0.0.1:1/v1")
        .with_api_key("sk-test")
        .list_models()
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("error") || msg.contains("refused") || msg.contains("reset"),
        "transport error expected, got: {msg}",
    );
}

//  Gemini

#[tokio::test]
async fn gemini_success() {
    let raw_body = r#"{"models":[{"name":"models/gemini-2.0-flash","version":"001"},{"name":"models/gemini-2.0-pro","version":"001"}]}"#;
    let (port, _body, _raw, handle) = mock_server(raw_body, 200);
    let result = Connector::new("gemini")
        .unwrap()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_api_key("gk-test")
        .list_models()
        .await;
    handle.join().unwrap();
    let out = result.unwrap();

    assert_eq!(out.raw(), raw_body);

    let ids: Vec<&str> = out.models().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gemini-2.0-flash", "gemini-2.0-pro"]);
}

#[tokio::test]
async fn gemini_http_401() {
    let (port, _body, _raw, handle) =
        mock_server(r#"{"error":{"message":"API key not valid"}}"#, 401);
    let err = Connector::new("gemini")
        .unwrap()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_api_key("gk-test")
        .list_models()
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("401"));
}

//  Claude

#[tokio::test]
async fn claude_success() {
    let raw_body = r#"{"data":[{"type":"model","id":"claude-sonnet-4-6"},{"type":"model","id":"claude-opus-4"}]}"#;
    let (port, _body, _raw, handle) = mock_server(raw_body, 200);
    let result = claude_connector(port).list_models().await;
    handle.join().unwrap();
    let out = result.unwrap();

    assert_eq!(out.raw(), raw_body);

    let ids: Vec<&str> = out.models().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-sonnet-4-6", "claude-opus-4"]);
}

#[tokio::test]
async fn claude_http_401() {
    let (port, _body, _raw, handle) = mock_server(r#"{"error":{"message":"Unauthorized"}}"#, 401);
    let err = claude_connector(port).list_models().await.unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("401"));
}
