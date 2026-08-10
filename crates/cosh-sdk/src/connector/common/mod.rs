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
        // An SSE event ends with a blank line. Both newline styles occur in
        // the wild: LF LF ("\n\n" — OpenAI, Claude) and CRLF CRLF
        // ("\r\n\r\n" — which the Gemini API emits even with `?alt=sse`).
        // Split on whichever separator comes FIRST: a parser that only
        // accepts "\n\n" never splits Gemini's CRLF stream, the whole body
        // stays buffered, and the caller silently receives zero frames (no
        // tokens, no error) — the "provider fails silently" symptom.
        while let Some(sep_end) = next_sse_separator(&self.buf) {
            let raw: Vec<u8> = self.buf.drain(..sep_end).collect();
            let text = String::from_utf8_lossy(&raw);
            if let Some(data) = text.lines().find_map(|l| l.strip_prefix("data: ")) {
                frames.push(data.to_owned());
            }
        }
        frames
    }

    /// Flush any remaining buffered data as frames at end-of-stream.
    ///
    /// The SSE spec dispatches pending data when the stream ends, so a
    /// server that closes the connection right after the last `data:` line
    /// (no trailing blank line) must not lose its final frame silently —
    /// the same class of failure as the CRLF separator bug. A no-op when
    /// the stream ended cleanly (every event was blank-line terminated).
    ///
    /// Note: a TRUNCATED stream (connection cut mid-frame) leaves a partial
    /// line here, which is emitted as-is and will fail JSON parsing at the
    /// consumer — surfacing the truncation as an error is intentional and
    /// preferable to silently dropping the tail.
    pub(crate) fn flush(&mut self) -> Vec<String> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let raw: Vec<u8> = std::mem::take(&mut self.buf);
        let text = String::from_utf8_lossy(&raw);
        text.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(String::from)
            .collect()
    }
}

/// Byte offset just past the first SSE event separator in `buf` — the blank
/// line between events, written as either LF LF ("\n\n") or CRLF CRLF
/// ("\r\n\r\n"). The two patterns never overlap (CRLF contains no "\n\n"
/// and vice versa), so taking the earliest match is unambiguous.
fn next_sse_separator(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n");
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => Some((a + 2).min(b + 4)),
        (Some(a), None) => Some(a + 2),
        (None, Some(b)) => Some(b + 4),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{SseBuffer, next_sse_separator};

    #[test]
    fn splits_lf_frames() {
        let mut buf = SseBuffer::new();
        let frames = buf.push_and_drain(
            b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n",
        );
        assert_eq!(frames, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    /// The Gemini API emits CRLF CRLF between SSE events — the exact wire
    /// format captured from a live `streamGenerateContent?alt=sse` call.
    /// A parser that only splits on LF LF would buffer the whole body and
    /// emit zero frames (the silent-failure bug).
    #[test]
    fn splits_crlf_frames() {
        let mut buf = SseBuffer::new();
        let frames = buf.push_and_drain(
            b"data: {\"a\":1}\r\n\r\ndata: {\"b\":2}\r\n\r\n",
        );
        assert_eq!(frames, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn buffers_partial_frame_until_separator() {
        let mut buf = SseBuffer::new();
        // First chunk ends mid-frame (no blank line yet).
        assert!(buf.push_and_drain(b"data: {\"a\":1}\r\n").is_empty());
        // The rest completes the frame and carries the next one.
        let frames = buf.push_and_drain(b"\r\ndata: {\"b\":2}\r\n\r\n");
        assert_eq!(frames, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn mixed_separator_styles_are_both_split() {
        let mut buf = SseBuffer::new();
        let frames = buf.push_and_drain(b"data: {\"a\":1}\n\ndata: {\"b\":2}\r\n\r\n");
        assert_eq!(frames, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn separator_offsets_are_correct() {
        assert_eq!(next_sse_separator(b"data: x\n\n"), Some(b"data: x\n\n".len()));
        assert_eq!(next_sse_separator(b"data: x\r\n\r\n"), Some(b"data: x\r\n\r\n".len()));
        assert_eq!(next_sse_separator(b"data: x"), None);
    }

    /// A stream that ends without a trailing blank line must still deliver
    /// its final frame at EOF — the "silently lost tail frame" class of
    /// failure (same family as the CRLF separator bug).
    #[test]
    fn flush_emits_final_frame_without_separator() {
        let mut buf = SseBuffer::new();
        // First frame ends cleanly; the second never gets its blank line,
        // so it must be recovered by flush() at EOF.
        let frames = buf.push_and_drain(b"data: {\"a\":1}\r\n\r\ndata: {\"b\":2}");
        assert_eq!(frames, vec![r#"{"a":1}"#]);
        assert_eq!(buf.flush(), vec![r#"{"b":2}"#]);
        assert!(buf.flush().is_empty());
    }

    #[test]
    fn flush_is_noop_when_stream_ended_cleanly() {
        let mut buf = SseBuffer::new();
        let frames = buf.push_and_drain(b"data: {\"a\":1}\n\n");
        assert_eq!(frames, vec![r#"{"a":1}"#]);
        assert!(buf.flush().is_empty());
    }

    #[test]
    fn flush_recovers_multiple_unseparated_data_lines() {
        // Server that never emits blank lines (NDJSON-ish): every `data:`
        // line still becomes a frame at EOF.
        let mut buf = SseBuffer::new();
        assert!(buf.push_and_drain(b"data: {\"a\":1}\ndata: {\"b\":2}").is_empty());
        assert_eq!(buf.flush(), vec![r#"{"a":1}"#, r#"{"b":2}"#]);
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
