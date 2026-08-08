use super::error::ConnectorError;
use super::provider::ProviderConfig;
use std::sync::OnceLock;
use std::time::Duration;

/// Per-chunk read timeout for SSE streams. Generous (120s) so a slow local
/// model or a reasoning pause between tokens does not abort the agent loop
/// mid-generation; only a truly dead connection trips it.
pub(crate) const SSE_CHUNK_TIMEOUT: Duration = Duration::from_secs(120);

/// Shared [`reqwest::Client`] reused across every provider request.
///
/// Creating a fresh client per request discards the connection pool, so every
/// LLM call pays a full DNS + TCP + TLS handshake — the dominant source of the
/// slow intervals between agent-loop calls. One long-lived client keeps
/// keep-alive connections alive between calls (and between sessions).
fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // Only a CONNECT timeout lives on the client: an SSE stream's body is
        // read incrementally by the caller, so a total request timeout here
        // would kill long generations. Per-chunk SSE timeouts govern the body.
        reqwest::Client::builder()
            .pool_max_idle_per_host(4)
            .connect_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    pub(crate) const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub(crate) fn push_and_drain(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let raw: Vec<u8> = self.buf.drain(..=end + 1).collect();
            let text = String::from_utf8_lossy(&raw[..raw.len().saturating_sub(2)]);
            if let Some(data) = text.lines().find_map(|l| l.strip_prefix("data: ")) {
                frames.push(data.to_owned());
            }
        }
        frames
    }
}

pub async fn send_request(
    config: &ProviderConfig,
    url: &str,
    body: &(impl serde::Serialize + Sync),
    headers: &[(&str, &str)],
) -> Result<String, ConnectorError> {
    let response = send_request_stream(config, url, body, headers).await?;
    // Non-streaming call: the whole body must arrive within the request
    // timeout (the stream path governs the body via per-chunk timeouts).
    let text = tokio::time::timeout(Duration::from_mins(1), response.text())
        .await
        .map_err(|_| ConnectorError::Network("request timed out after 60s".to_string()))??;
    Ok(text)
}

pub async fn send_get_request(
    config: &ProviderConfig,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<String, ConnectorError> {
    let mut request_builder = shared_client().get(url).timeout(Duration::from_mins(1));

    for &(key, value) in headers {
        request_builder = request_builder.header(key, value);
    }

    if config.needs_extra_headers {
        request_builder = request_builder
            .header("HTTP-Referer", "https://localhost")
            .header("X-Title", "provider");
    }

    let response = request_builder.send().await?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unable to read error response".to_string());
        let status = status.as_u16();
        let error_msg = format!("HTTP {status} - {error_text}");
        log::error!("HTTP Error captured: {error_msg}");
        return Err(ConnectorError::classify_http(status, error_text));
    }

    Ok(response.text().await?)
}

pub async fn send_request_stream(
    config: &ProviderConfig,
    url: &str,
    body: &(impl serde::Serialize + Sync),
    headers: &[(&str, &str)],
) -> Result<reqwest::Response, ConnectorError> {
    let mut request_builder = shared_client()
        .post(url)
        .header("Content-Type", "application/json");

    for &(key, value) in headers {
        request_builder = request_builder.header(key, value);
    }

    if config.needs_extra_headers {
        request_builder = request_builder
            .header("HTTP-Referer", "https://localhost")
            .header("X-Title", "provider");
    }

    let json_body = serde_json::to_string(body)?;
    let response = tokio::time::timeout(
        Duration::from_mins(1),
        request_builder.body(json_body).send(),
    )
    .await
    .map_err(|_| ConnectorError::Network("request timed out after 60s".to_string()))?
    .map_err(ConnectorError::from)?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unable to read error response".to_string());
        let status = status.as_u16();
        let error_msg = format!("HTTP {status} - {error_text}");
        log::error!("HTTP Error captured: {error_msg}");
        return Err(ConnectorError::classify_http(status, error_text));
    }

    Ok(response)
}
