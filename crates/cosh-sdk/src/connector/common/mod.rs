use super::error::ConnectorError;
use super::provider::ProviderConfig;

pub(crate) struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    pub(crate) fn new() -> Self {
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

pub(crate) async fn send_request(
    config: &ProviderConfig,
    url: &str,
    body: &(impl serde::Serialize + Sync),
    headers: &[(&str, &str)],
) -> Result<String, ConnectorError> {
    let response = send_request_stream(config, url, body, headers).await?;
    Ok(response.text().await?)
}

pub(crate) async fn send_get_request(
    config: &ProviderConfig,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<String, ConnectorError> {
    let client = reqwest::Client::new();
    let mut request_builder = client.get(url);

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
        let error_msg = format!("HTTP {} - {}", status.as_u16(), error_text);
        log::error!("HTTP Error captured: {error_msg}");
        return Err(ConnectorError::HttpError {
            status: status.as_u16(),
            body: error_text,
        });
    }

    Ok(response.text().await?)
}

pub(crate) async fn send_request_stream(
    config: &ProviderConfig,
    url: &str,
    body: &(impl serde::Serialize + Sync),
    headers: &[(&str, &str)],
) -> Result<reqwest::Response, ConnectorError> {
    let client = reqwest::Client::new();
    let mut request_builder = client.post(url).header("Content-Type", "application/json");

    for &(key, value) in headers {
        request_builder = request_builder.header(key, value);
    }

    if config.needs_extra_headers {
        request_builder = request_builder
            .header("HTTP-Referer", "https://localhost")
            .header("X-Title", "provider");
    }

    let json_body = serde_json::to_string(body)?;
    let response = request_builder.body(json_body).send().await?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unable to read error response".to_string());
        let error_msg = format!("HTTP {} - {}", status.as_u16(), error_text);
        log::error!("HTTP Error captured: {error_msg}");
        return Err(ConnectorError::HttpError {
            status: status.as_u16(),
            body: error_text,
        });
    }

    Ok(response)
}
