use super::{EmbedParams, Parameters, ResponseFormat, embed, openai};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Saves and restores an env var within its scope.
struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    fn remove(key: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
        Self {
            key: key.to_owned(),
            old,
        }
    }
    fn set(key: &str, val: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val) };
        Self {
            key: key.to_owned(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => unsafe { std::env::set_var(&self.key, v) },
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Starts a mock HTTP server. Returns `(port, request_body, raw_http, handle)`.
fn mock_server(
    response: &str,
    status_code: u16,
) -> (
    u16,
    Arc<Mutex<Option<String>>>,
    Arc<Mutex<Option<String>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = response.to_owned();
    let captured_body = Arc::new(Mutex::new(None::<String>));
    let captured_body_clone = Arc::clone(&captured_body);
    let captured_raw = Arc::new(Mutex::new(None::<String>));
    let captured_raw_clone = Arc::clone(&captured_raw);

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();

        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];

        // Read full request, then store raw and parse body.
        loop {
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);

            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..pos]);
                let content_len: usize = headers
                    .lines()
                    .filter_map(|l| {
                        l.trim()
                            .to_lowercase()
                            .strip_prefix("content-length:")?
                            .trim()
                            .parse()
                            .ok()
                    })
                    .next()
                    .unwrap_or(0);
                let body_start = pos + 4;
                if buf.len() >= body_start + content_len {
                    let req_body =
                        String::from_utf8_lossy(&buf[body_start..body_start + content_len])
                            .to_string();
                    *captured_body_clone.lock().unwrap() = Some(req_body);
                    let raw = String::from_utf8_lossy(&buf[..body_start + content_len]).to_string();
                    *captured_raw_clone.lock().unwrap() = Some(raw);
                    break;
                }
            }
        }

        let status_line = if status_code == 200 {
            format!("HTTP/1.1 {status_code} OK")
        } else {
            format!("HTTP/1.1 {status_code} Error")
        };
        let resp = format!(
            "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    (port, captured_body, captured_raw, handle)
}

fn mock_params(port: u16) -> Parameters {
    Parameters::default()
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_api_key("sk-test")
}

fn mock_base_url(port: u16) -> Parameters {
    Parameters::default().with_base_url(format!("http://127.0.0.1:{port}/v1"))
}

fn mock_embed_params(port: u16) -> EmbedParams {
    EmbedParams::default()
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_api_key("sk-test")
}

// openai() — error propagation

#[tokio::test]
async fn test_unknown_provider_returns_clear_error() {
    let err = openai("nonexistent-provider", "hello", None, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Unknown provider");
}

#[tokio::test]
async fn test_missing_api_key_returns_descriptive_error() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let err = openai("openai", "hello", None, None).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("API key not set for provider: openai"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn test_http_401_propagates_error_body() {
    let (port, _body, _raw, handle) = mock_server(
        r#"{"error":{"message":"Incorrect API key","type":"invalid_request_error"}}"#,
        401,
    );
    let err = openai("openai", "hello", None, Some(mock_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("401"), "status code missing from: {msg}");
    assert!(
        msg.contains("Incorrect API key"),
        "server detail missing from: {msg}"
    );
}

#[tokio::test]
async fn test_http_500_propagates_status_code() {
    let (port, _body, _raw, handle) = mock_server("Internal Server Error", 500);
    let err = openai("openai", "hello", None, Some(mock_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn test_malformed_json_response_returns_deserialization_error() {
    let (port, _body, _raw, handle) = mock_server("not-json-at-all", 200);
    let err = openai("openai", "hello", None, Some(mock_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("expected") || msg.contains("invalid"),
        "deserialization error expected, got: {msg}"
    );
}

#[tokio::test]
async fn test_empty_choices_array_returns_no_choices_error() {
    let (port, _body, _raw, handle) = mock_server(r#"{"choices":[]}"#, 200);
    let err = openai("openai", "hello", None, Some(mock_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.to_string(), "No choices in response");
}

#[tokio::test]
async fn test_null_content_in_choice_returns_no_content_error() {
    let (port, _body, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":null}}]}"#, 200);
    let err = openai("openai", "hello", None, Some(mock_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.to_string(), "No content in response");
}

#[tokio::test]
async fn test_network_error_when_server_unreachable() {
    let params = Parameters::default()
        .with_base_url("http://127.0.0.1:1/v1".to_string())
        .with_api_key("sk-test");
    let err = openai("openai", "hello", None, Some(params))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("error") || msg.contains("refused") || msg.contains("reset"),
        "transport error expected, got: {msg}"
    );
}

// openai() — successful response

#[tokio::test]
async fn test_successful_response_returns_content() {
    let (port, _body, _raw, handle) = mock_server(
        r#"{"choices":[{"message":{"content":"Hello world"}}]}"#,
        200,
    );
    let result = openai(
        "openai",
        "hello",
        Some("Be concise"),
        Some(mock_params(port)),
    )
    .await;
    handle.join().unwrap();
    assert_eq!(result.unwrap(), "Hello world");
}

// openai() — request body composition

#[tokio::test]
async fn test_system_prompt_appears_in_request_body_as_system_role() {
    let (port, captured, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let result = openai(
        "openai",
        "user text",
        Some("system text"),
        Some(mock_params(port)),
    )
    .await;
    handle.join().unwrap();
    assert!(result.is_ok());

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "system text");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "user text");
}

#[tokio::test]
async fn test_params_are_serialized_in_request_body() {
    let (port, captured, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let params = mock_params(port)
        .with_model("gpt-4")
        .with_max_tokens(500)
        .with_temperature(0.7)
        .with_seed(42);
    let _ = openai("openai", "hello", None, Some(params)).await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["model"], "gpt-4");
    assert_eq!(json["max_tokens"], 500);
    assert!((json["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
    assert_eq!(json["seed"], 42);
}

#[tokio::test]
async fn test_params_additional_fields_are_serialized() {
    let (port, captured, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let params = mock_params(port)
        .with_top_p(0.9)
        .with_stop(serde_json::json!(["END", "STOP"]))
        .with_frequency_penalty(0.5)
        .with_presence_penalty(-0.5)
        .with_response_format(ResponseFormat::json_object())
        .with_logprobs(true)
        .with_top_logprobs(5)
        .with_user("test-user");
    let _ = openai("openai", "hello", None, Some(params)).await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!((json["top_p"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(json["stop"], serde_json::json!(["END", "STOP"]));
    assert!((json["frequency_penalty"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert!((json["presence_penalty"].as_f64().unwrap() - (-0.5)).abs() < 1e-6);
    assert_eq!(json["response_format"]["type"], "json_object");
    assert!(json["logprobs"].as_bool().unwrap());
    assert_eq!(json["top_logprobs"], 5);
    assert_eq!(json["user"], "test-user");
}

#[tokio::test]
async fn test_model_fallback_to_default_model() {
    let (port, captured, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let params = mock_params(port);
    let _ = openai("openai", "hello", None, Some(params)).await;
    handle.join().unwrap();

    let body = captured.lock().unwrap().take().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["model"], "gpt-4o-mini");
}

// openai() — extra headers (openrouter)

#[tokio::test]
async fn test_openrouter_extra_headers_are_sent() {
    let (port, _body, raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let params = Parameters::default()
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_api_key("sk-test");
    let _ = openai("openrouter", "hello", None, Some(params)).await;
    handle.join().unwrap();

    let req = raw.lock().unwrap().take().unwrap().to_lowercase();
    assert!(req.contains("http-referer:"), "missing http-referer header");
    assert!(req.contains("x-title:"), "missing x-title header");
}

// openai() — api_key resolution

#[tokio::test]
async fn test_api_key_env_var_fallback_succeeds() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::set("OPENAI_API_KEY", "sk-from-env");
    let (port, _body, _raw, handle) =
        mock_server(r#"{"choices":[{"message":{"content":"ok"}}]}"#, 200);
    let result = openai("openai", "hello", None, Some(mock_base_url(port))).await;
    handle.join().unwrap();
    assert!(result.is_ok());
}

// embed() — error propagation

#[tokio::test]
async fn test_embed_unknown_provider_returns_clear_error() {
    let err = embed("nonexistent-provider", "text", None, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Unknown provider");
}

#[tokio::test]
async fn test_embed_missing_api_key_returns_descriptive_error() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let err = embed("openai", "text", None, None).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("API key not set for provider: openai"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn test_embed_http_error_propagates_status_code() {
    let (port, _body, _raw, handle) = mock_server("error", 500);
    let err = embed("openai", "text", None, Some(mock_embed_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn test_embed_malformed_json_response_returns_error() {
    let (port, _body, _raw, handle) = mock_server("not-json", 200);
    let err = embed("openai", "text", None, Some(mock_embed_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("expected") || msg.contains("invalid"),
        "deserialization error expected, got: {msg}"
    );
}

#[tokio::test]
async fn test_embed_empty_data_array_returns_no_embeddings_error() {
    let (port, _body, _raw, handle) = mock_server(r#"{"data":[]}"#, 200);
    let err = embed("openai", "text", None, Some(mock_embed_params(port)))
        .await
        .unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.to_string(), "No embeddings in response");
}

// embed() — successful response

#[tokio::test]
async fn test_embed_successful_response_returns_vector() {
    let (port, _body, _raw, handle) = mock_server(r#"{"data":[{"embedding":[0.1,0.2,0.3]}]}"#, 200);
    let result = embed("openai", "text", None, Some(mock_embed_params(port))).await;
    handle.join().unwrap();
    assert_eq!(result.unwrap(), vec![0.1, 0.2, 0.3]);
}
