use super::error::ConnectorError;
use super::params::ClaudeThinkingBlock;
use super::provider::Family;
use super::usage::TokenUsage;
use crate::extract_action::NativeToolCall;
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

    /// The REAL cost (USD) the provider reported inside this response, if
    /// any.
    ///
    /// Same wire shapes as [`ChatStream::reported_cost`]: `usage.cost`
    /// (OpenRouter, Charm Hyper) or a top-level `cost` — numeric or numeric
    /// string — as the OpenCode Zen/Go gateways send. This is the
    /// authoritative billed amount; `None` means the provider reported no
    /// cost and the record stays unpriced (no local fallback).
    #[must_use]
    pub fn reported_cost(&self) -> Option<f64> {
        extract_reported_cost(&self.raw)
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
    pub(crate) reasoning: String,
    pub(crate) finish_reason: Option<String>,
    /// Claude extended-thinking blocks completed in this turn (text +
    /// signature), carried verbatim so the harness can replay them on the
    /// follow-up request. `None` for every other provider and for turns
    /// without thinking blocks.
    pub(crate) thinking_blocks: Option<Vec<ClaudeThinkingBlock>>,
    /// A tool call the provider delivered NATIVELY (structured
    /// `tool_calls`/`tool_use`/`functionCall` parts) — the harness routes it
    /// straight to the extractor's native validation funnel instead of
    /// parsing text. `None` for text/reasoning chunks and for the legacy
    /// inline-JSON path (which arrives as `token` text).
    pub(crate) tool_call: Option<NativeToolCall>,
    /// Set on the marker chunk the retry middleware emits BEFORE re-streaming
    /// a response after a mid-stream failure: everything the consumer
    /// buffered so far belongs to the failed attempt and must be discarded
    /// (see [`StreamChunk::reset`]).
    pub(crate) reset: bool,
}

impl StreamChunk {
    /// Marker chunk the retry middleware emits before re-streaming a
    /// response after a mid-stream failure: the consumer must discard any
    /// partial content buffered from the failed attempt (the retried
    /// response restarts from the beginning).
    #[must_use]
    pub fn reset() -> Self {
        Self {
            raw: String::new(),
            token: String::new(),
            reasoning: String::new(),
            finish_reason: None,
            thinking_blocks: None,
            tool_call: None,
            reset: true,
        }
    }

    /// Whether this chunk is a retry marker (see [`StreamChunk::reset`]).
    #[must_use]
    pub fn is_reset(&self) -> bool {
        self.reset
    }
}

impl StreamChunk {
    /// Reasoning/thinking delta for this chunk (if the provider streams it).
    ///
    /// Many reasoning models emit this separately from the visible text
    /// token. It is shown in the TUI as a collapsible "Thought" block but is
    /// never echoed back to the model.
    #[must_use]
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }
    /// Text delta for this chunk — the next token(s) in the model's reply.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Claude extended-thinking blocks completed in this turn, replayed
    /// verbatim on the follow-up request so the Anthropic API can validate
    /// the reasoning signatures. Only the Claude caller populates this;
    /// other providers always yield `None`.
    #[must_use]
    pub fn thinking_blocks(&self) -> Option<&[ClaudeThinkingBlock]> {
        self.thinking_blocks.as_deref()
    }

    /// Why the stream finished, if this chunk is the last one.
    ///
    /// Possible values: `"stop"`, `"length"`, `"content_filter"`, `"tool_calls"`,
    /// or provider-specific values. Returns `None` for intermediate chunks.
    #[must_use]
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// The native tool call carried by this chunk, if the provider
    /// delivered one structurally (`tool_calls`/`tool_use`/`functionCall`).
    /// The harness feeds it to the extractor's native validation funnel
    /// (see `ExtractAction::register_native_call`) — no text parsing, the
    /// provider already split the call into structured fields.
    #[must_use]
    pub fn tool_call(&self) -> Option<&NativeToolCall> {
        self.tool_call.as_ref()
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
    /// Token usage accumulated across the stream's frames (per-field max
    /// merge — see [`TokenUsage::merge_stream`]). Cleared on the retry
    /// middleware's reset marker so a re-streamed attempt starts fresh.
    usage: Option<TokenUsage>,
    /// REAL cost (USD) reported by the provider itself inside the response
    /// (`usage.cost` — OpenRouter, Vercel AI Gateway; a top-level `cost` on
    /// the final frame — OpenCode Zen/Go). This is the authoritative billed
    /// amount and always supersedes any local price-table estimate.
    /// Gateways that do not report a cost leave this `None`. Cleared on the
    /// retry middleware's reset marker.
    reported_cost: Option<f64>,
    family: Family,
    finished: bool,
}

impl ChatStream {
    pub(crate) fn new(
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ConnectorError>> + Send>>,
        family: Family,
    ) -> Self {
        Self {
            inner,
            last_raw: None,
            usage: None,
            reported_cost: None,
            family,
            finished: false,
        }
    }

    /// Returns the token usage accumulated from the stream's usage frames.
    ///
    /// If the stream hasn't been fully consumed yet, this drains any
    /// remaining items first. The counters are normalized onto
    /// [`TokenUsage`] (including prompt-cache accounting) and reflect the
    /// provider's OWN usage object — the same numbers the API bills, never
    /// a local estimate.
    ///
    /// Returns `None` when the provider never reported usage (e.g. the
    /// stream failed before a usage frame arrived).
    pub async fn usage(&mut self) -> Option<TokenUsage> {
        if !self.finished {
            use tokio_stream::StreamExt;
            while self.next().await.is_some() {}
        }
        self.usage
    }

    /// Returns the REAL cost (USD) the provider reported inside the
    /// response, if any.
    ///
    /// Wire shapes recognized: `usage.cost` (OpenRouter, Charm Hyper)
    /// and the OpenCode Zen/Go gateways' trailing top-level `cost` —
    /// numeric or numeric string — on the final streamed frame. This is the
    /// authoritative spend figure. Providers that do not report a cost
    /// return `None` — the record stays unpriced; nothing is estimated
    /// locally.
    pub async fn reported_cost(&mut self) -> Option<f64> {
        if !self.finished {
            use tokio_stream::StreamExt;
            while self.next().await.is_some() {}
        }
        self.reported_cost
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
            if chunk.is_reset() {
                // The retry middleware is re-streaming a failed attempt from
                // the beginning: drop the failed attempt's partial usage.
                self.usage = None;
                self.reported_cost = None;
            } else {
                if let Some(u) = extract_family_usage(self.family, &chunk.raw) {
                    self.usage = Some(match self.usage {
                        Some(prev) => prev.merge_stream(u),
                        None => u,
                    });
                }
                // Reported cost only ever arrives on the FINAL frame and only
                // grows within one request, so keep the max seen. Stays `None`
                // for providers that do not report a cost.
                let mut cost = self.reported_cost;
                if let Some(c) = extract_reported_cost(&chunk.raw) {
                    cost = Some(cost.map_or(c, |prev| prev.max(c)));
                    self.reported_cost = cost;
                }
                // Keep the usage object's copy in sync so `usage()`
                // consumers (the TUI usage panel) see the real billed
                // amount without a second accessor call.
                if let Some(u) = &mut self.usage {
                    u.reported_cost = cost;
                }
            }
            self.last_raw = Some(chunk.raw.clone());
        }
        if matches!(poll, Poll::Ready(None)) {
            self.finished = true;
        }
        poll
    }
}

/// Extract the normalized [`TokenUsage`] from a raw SSE frame using the
/// family-specific field shapes (`usage` vs `usageMetadata`, cache detail
/// paths, …).
fn extract_family_usage(family: Family, raw: &str) -> Option<TokenUsage> {
    match family {
        Family::OpenAICompatible => super::openai_compatible::extract_usage(raw),
        Family::OpenAi => super::openai::extract_usage(raw),
        Family::Claude => super::claude::extract_usage(raw),
        Family::Gemini => super::gemini::extract_usage(raw),
    }
}

/// Extract the REAL billed cost (USD) from a raw response/SSE frame.
///
/// Two wire shapes are recognized, both carrying what the gateway actually
/// charged:
/// - `usage.cost` (a number inside the usage object) — OpenRouter documents
///   this field; the Vercel AI Gateway follows the same shape.
/// - a TOP-LEVEL `cost` — how the OpenCode Zen/Go gateways report it: the
///   streamed chat completion ends with a final frame
///   `{"choices":[],"cost":"0"}` outside the usage object, typed as a JSON
///   string (observed on the wire; see anomalyco/opencode issues #42918 and
///   #26213). Numeric strings are accepted, so `"cost":"0.0042"` and
///   `"cost":0.0042` both parse.
///
/// xAI reports the exact billed amount as `usage.cost_in_usd_ticks`
/// (1 USD = 10^10 ticks); it is converted to USD here so every consumer
/// sees one unit.
///
/// Family-agnostic on purpose. Families whose native response shape has no
/// `usage`/`cost` object (Claude, Gemini) simply never match.
///
/// Rejects negative or non-finite/non-numeric values: a malformed cost must
/// never poison the spend totals.
pub(crate) fn extract_reported_cost(raw: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let ticks = v
        .get("usage")
        .and_then(|usage| usage.get("cost_in_usd_ticks"))
        .and_then(serde_json::Value::as_i64);
    if let Some(ticks) = ticks {
        // xAI bills in 10^-10 USD ticks (docs.x.ai/developers/cost-tracking).
        let usd = ticks as f64 / 10_000_000_000.0;
        return if usd.is_finite() && usd >= 0.0 {
            Some(usd)
        } else {
            None
        };
    }
    let value = v
        .get("usage")
        .and_then(|usage| usage.get("cost"))
        .or_else(|| v.get("cost"))?;
    let cost = match value {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if cost.is_finite() && cost >= 0.0 {
        Some(cost)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_reported_cost_from_openai_shape() {
        let raw = r#"{"usage":{"prompt_tokens":194,"completion_tokens":2,"cost":0.95}}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.95));
    }

    #[test]
    fn extracts_reported_cost_from_openrouter_streaming_frame() {
        let raw = r#"{"object":"chat.completion.chunk","usage":{"prompt_tokens":194,"completion_tokens":2,"cost":0.95,"cost_details":{"upstream_inference_cost":19},"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":100}}}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.95));
    }

    /// The OpenCode Zen/Go gateways end the stream with a non-standard
    /// trailer carrying a TOP-LEVEL `cost`, typed as a JSON string — the
    /// exact frame captured on the wire (anomalyco/opencode#42918).
    #[test]
    fn extracts_reported_cost_from_opencode_top_level_string() {
        let raw = r#"{"choices":[],"cost":"0"}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.0));
        let raw = r#"{"choices":[],"cost":"0.0042"}"#;
        assert!((extract_reported_cost(raw).unwrap() - 0.0042).abs() < 1e-12);
        // Numeric form of the same field (in case the gateway tightens the
        // type later).
        let raw = r#"{"choices":[],"cost":0.0042}"#;
        assert!((extract_reported_cost(raw).unwrap() - 0.0042).abs() < 1e-12);
        // Non-streaming responses report the same top-level shape.
        let raw = r#"{"id":"x","choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":8,"completion_tokens":6},"cost":"0.0031"}"#;
        assert!((extract_reported_cost(raw).unwrap() - 0.0031).abs() < 1e-12);
    }

    /// xAI: the exact billed amount arrives as `usage.cost_in_usd_ticks`
    /// (1 USD = 10^10 ticks) and is converted to USD here.
    #[test]
    fn extracts_reported_cost_from_xai_ticks() {
        // 37_756_000 ticks (docs.x.ai example, printed there as $0.0038).
        let raw = r#"{"usage":{"input_tokens":199,"output_tokens":1,"cost_in_usd_ticks":37756000}}"#;
        assert!((extract_reported_cost(raw).unwrap() - 0.0037756).abs() < 1e-12);
        // Streaming final chunk carries the same field.
        let raw = r#"{"choices":[],"usage":{"cost_in_usd_ticks":158500}}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.00001585));
    }

    /// `usage.cost` wins when both shapes appear in one frame (the usage
    /// field is the more specific location).
    #[test]
    fn usage_cost_takes_precedence_over_top_level() {
        let raw = r#"{"usage":{"prompt_tokens":8,"cost":0.95},"cost":"0"}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.95));
    }

    #[test]
    fn zero_reported_cost_is_valid() {
        // Free-tier models genuinely report $0 — this MUST be kept so the
        // estimate path does not bill a free model.
        let raw = r#"{"usage":{"prompt_tokens":10,"completion_tokens":2,"cost":0}}"#;
        assert_eq!(extract_reported_cost(raw), Some(0.0));
    }

    #[test]
    fn missing_cost_or_usage_is_none() {
        assert_eq!(
            extract_reported_cost(r#"{"usage":{"prompt_tokens":1}}"#),
            None
        );
        assert_eq!(extract_reported_cost(r#"{"choices":[]}"#), None);
        assert_eq!(extract_reported_cost("not json"), None);
    }

    #[test]
    fn non_numeric_or_invalid_cost_is_none() {
        // Non-numeric strings are not costs.
        assert_eq!(extract_reported_cost(r#"{"usage":{"cost":"free"}}"#), None);
        assert_eq!(extract_reported_cost(r#"{"cost":true}"#), None);
        assert_eq!(extract_reported_cost(r#"{"usage":{"cost":-1.0}}"#), None);
        let nan = f64::NAN;
        let raw = format!(r#"{{"usage":{{"cost":{nan}}}}}"#);
        assert_eq!(extract_reported_cost(&raw), None);
    }

    /// Numeric strings parse: the OpenCode gateways type `cost` as a JSON
    /// string on the wire (#42918), which must not fall back to estimation.
    #[test]
    fn numeric_string_cost_is_valid() {
        assert_eq!(
            extract_reported_cost(r#"{"usage":{"cost":"0.95"}}"#),
            Some(0.95)
        );
    }

    #[tokio::test]
    async fn reported_cost_accumulates_across_frames_and_survives_drain() {
        use tokio_stream::StreamExt;
        let frame = |cost: f64| {
            Ok(StreamChunk {
                raw: format!(
                    r#"{{"usage":{{"prompt_tokens":10,"completion_tokens":2,"cost":{cost}}}}}"#
                ),
                token: String::new(),
                reasoning: String::new(),
                finish_reason: None,
                thinking_blocks: None,
                tool_call: None,
                reset: false,
            })
        };
        let frames = vec![frame(0.4), frame(0.95)];
        let mut stream = ChatStream::new(
            Box::pin(tokio_stream::iter(frames)),
            Family::OpenAICompatible,
        );
        while stream.next().await.is_some() {}
        // Final-frame cost wins (per-request cost only grows).
        assert_eq!(stream.reported_cost().await, Some(0.95));
    }

    #[tokio::test]
    async fn reported_cost_is_cleared_on_retry_reset() {
        use tokio_stream::StreamExt;
        let cost_frame = |reset: bool| {
            Ok(StreamChunk {
                raw: r#"{"usage":{"prompt_tokens":10,"completion_tokens":2,"cost":0.95}}"#.into(),
                token: String::new(),
                reasoning: String::new(),
                finish_reason: None,
                thinking_blocks: None,
                tool_call: None,
                reset,
            })
        };
        // A failed attempt whose partial cost arrived before the reset marker
        // must NOT leak into the retried response (or it would be counted
        // twice once the retry's own final frame lands).
        let frames = vec![cost_frame(false), Ok(StreamChunk::reset())];
        let mut stream = ChatStream::new(
            Box::pin(tokio_stream::iter(frames)),
            Family::OpenAICompatible,
        );
        while stream.next().await.is_some() {}
        assert_eq!(stream.reported_cost().await, None);
        assert_eq!(stream.usage().await, None);
    }

    /// End-to-end over the frames the OpenCode Zen/Go gateways actually
    /// send (`anomalyco/opencode#42918`): a usage-only frame followed by the
    /// non-standard `{"choices":[],"cost":"0"}` trailer. The REAL billed
    /// cost must surface both via `reported_cost()` and inside the
    /// `usage()` object — the TUI usage panel's single source.
    #[tokio::test]
    async fn opencode_cost_trailer_lands_in_stream_usage() {
        use tokio_stream::StreamExt;
        let frames = vec![
            Ok(StreamChunk {
                raw: r#"{"id":"","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hi!"},"finish_reason":null}]}"#.into(),
                token: "Hi!".into(),
                reasoning: String::new(),
                finish_reason: None,
                thinking_blocks: None,
                tool_call: None,
                reset: false,
            }),
            Ok(StreamChunk {
                raw: r#"{"id":"gen-...","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":6,"total_tokens":14}}"#.into(),
                token: String::new(),
                reasoning: String::new(),
                finish_reason: Some("stop".into()),
                thinking_blocks: None,
                tool_call: None,
                reset: false,
            }),
            // The gateway's cost trailer: no id/object/created/model.
            Ok(StreamChunk {
                raw: r#"{"choices":[],"cost":"0"}"#.into(),
                token: String::new(),
                reasoning: String::new(),
                finish_reason: None,
                thinking_blocks: None,
                tool_call: None,
                reset: false,
            }),
        ];
        let mut stream = ChatStream::new(
            Box::pin(tokio_stream::iter(frames)),
            Family::OpenAICompatible,
        );
        while stream.next().await.is_some() {}
        assert_eq!(stream.reported_cost().await, Some(0.0));
        let usage = stream.usage().await.unwrap();
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 6);
        assert_eq!(usage.reported_cost, Some(0.0));
    }
}

/// A single model entry returned by [`Connector::list_models`].
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
}

impl ModelInfo {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Result of a [`Connector::list_models`] call.
///
/// Provides the parsed model list via [`models`](LsOutput::models)
/// and the raw JSON response via [`raw`](LsOutput::raw).
#[derive(Debug)]
pub struct LsOutput {
    raw: String,
    models: Vec<ModelInfo>,
}

impl LsOutput {
    pub(crate) const fn new(raw: String, models: Vec<ModelInfo>) -> Self {
        Self { raw, models }
    }

    /// Raw JSON response from the API as received (unmodified).
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Extracted model list.
    #[must_use]
    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }
}
