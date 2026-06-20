use super::error::ConnectorError;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::Stream;

/// Result of a non-streaming chat completion.
///
/// Provides access to the parsed message text via [`message`](ChatOutput::message)
/// and the raw JSON response via [`raw`](ChatOutput::raw).
#[derive(Debug)]
pub struct ChatOutput {
    pub(crate) raw: String,
    pub(crate) message: String,
}

impl ChatOutput {
    /// Raw JSON response from the API as received (unmodified).
    ///
    /// Contains `usage`, `finish_reason`, `tool_calls`, and any other
    /// fields the API returned — parse it manually for data not covered
    /// by [`message`](Self::message).
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Extracted text content of the model's reply.
    ///
    /// This is what [`Connector::chat`](crate::connector::Connector::chat)
    /// returned before — the plain text of the assistant's message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A single chunk from a streaming chat response.
///
/// Each item in the stream carries the raw SSE frame and the extracted
/// text delta (token).
#[derive(Debug)]
pub struct StreamChunk {
    pub(crate) raw: String,
    pub(crate) token: String,
}

impl StreamChunk {
    /// Text delta for this chunk — the next token(s) in the model's reply.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// A streaming chat response that accumulates the last raw frame.
///
/// Yields [`StreamChunk`] items while the stream is active. After the
/// stream finishes, [`raw`](ChatStream::raw) returns the last SSE frame
/// received — typically the one carrying `usage`, `finish_reason`, and
/// other metadata.
///
/// # Example
///
/// ```ignore
/// let mut stream = connector.stream_chat("hello").await?;
/// while let Some(chunk) = stream.next().await {
///     let chunk = chunk?;
///     print!("{}", chunk.token());
/// }
/// let meta = stream.raw().await?; // last frame with usage, etc.
/// ```
pub struct ChatStream {
    inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ConnectorError>> + Send>>,
    last_raw: Option<String>,
    finished: bool,
}

impl ChatStream {
    pub(crate) fn new(
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ConnectorError>> + Send>>,
    ) -> Self {
        Self {
            inner,
            last_raw: None,
            finished: false,
        }
    }

    /// Returns the last raw SSE frame received from the stream.
    ///
    /// If the stream hasn't been fully consumed yet, this drains any
    /// remaining items before returning. The returned value is the
    /// last `data:` frame — typically containing `usage`, `finish_reason`,
    /// and the complete concatenated content (`OpenAI`) or the full
    /// response object (Gemini).
    ///
    /// # Errors
    ///
    /// Returns `StreamTerminated` if the stream ended without receiving
    /// any SSE frame (e.g., connection was reset before any data arrived).
    pub async fn raw(&mut self) -> Result<&str, ConnectorError> {
        if !self.finished {
            use tokio_stream::StreamExt;
            while self.next().await.is_some() {}
        }
        self.last_raw
            .as_deref()
            .ok_or(ConnectorError::StreamTerminated)
    }
}

impl Stream for ChatStream {
    type Item = Result<StreamChunk, ConnectorError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(Some(Ok(ref chunk))) = poll {
            self.last_raw = Some(chunk.raw.clone());
        }
        if let Poll::Ready(None) = poll {
            self.finished = true;
        }
        poll
    }
}
