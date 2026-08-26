use std::fmt;

/// Errors returned by the [`Connector`](crate::connector::Connector) API.
#[derive(Debug)]
pub enum ConnectorError {
    /// The provider name passed to [`Connector::new`](crate::connector::Connector::new) is not recognized.
    UnknownProvider(String),
    /// No API key was found for the provider (neither explicit nor in env vars).
    MissingApiKey(String),
    /// An anonymous OpenCode Zen request (sentinel `public` bearer) named a
    /// model outside the gateway's free tier. Fails fast client-side with a
    /// clear message instead of a bare 401 from the server; the fix is
    /// connecting a Zen API key or picking a `-free` model.
    AnonymousModelBlocked(String),
    /// The upstream API returned a non-2xx HTTP status.
    HttpError {
        /// HTTP status code (e.g., 401, 500).
        status: u16,
        /// Response body from the server.
        body: String,
        /// `retry-after` / `retry-after-ms` from the response headers, when
        /// the server sent one that parsed to a sane (0..=60s) delay. Used
        /// by the retry middleware to pace rate-limit retries instead of
        /// blindly backing off.
        retry_after_ms: Option<u64>,
        /// Whether the error is transient (e.g. OpenAI SSE `server_error`,
        /// `overloaded_error`) and safe to retry — even when the HTTP status
        /// is 200 (mid-stream error event). Mirrors fantasy's
        /// `TransientStreamErrorTypes` map.
        transient: bool,
    },
    /// The upstream rejected the request because the prompt exceeds the
    /// model's context window (OpenAI/Anthropic-style 400 with a
    /// "context length" body, or a 413 payload-too-large). The reliable
    /// signal is the BODY, not the status: SSE error frames and
    /// OpenAI-compatible JSON error responses arrive with status 200.
    ContextWindowExceeded {
        /// HTTP status code (e.g. 400, 413, or 200 for API-error frames).
        status: u16,
        /// Response body from the server.
        body: String,
        /// The model's context window in tokens, when the provider reports
        /// it in the body (e.g. "maximum context length is 128000"). Only a
        /// heuristic hint for local budget checks — never a hard contract.
        window_tokens: Option<usize>,
    },
    /// Failed to parse the response body as JSON.
    Deserialization(String),
    /// A network-level error occurred (connection refused, timeout, etc.).
    Network(String),
    /// The chat response contained an empty `choices` array.
    NoChoices,
    /// The chat response had no `content` field in the selected choice.
    NoContent,
    /// The embedding response contained an empty `data` array.
    NoEmbeddings,
    /// An operation is not yet implemented for the given provider.
    NotImplemented(&'static str),
    /// The SSE stream ended without a `[DONE]` signal.
    StreamTerminated,
}

impl ConnectorError {
    /// Classify an HTTP error response: [`ConnectorError::ContextWindowExceeded`]
    /// when the body carries a context-window overflow marker (the reliable
    /// signal across OpenAI/Anthropic/Gemini and OpenAI-compatible proxies),
    /// otherwise a plain [`ConnectorError::HttpError`].
    ///
    /// Every non-2xx path in the SDK funnels through this constructor, so a
    /// context-window overflow is never mistaken for an unrelated 400 (bad
    /// JSON, invalid tool argument, ...) and never loses the window size.
    pub fn classify_http(status: u16, body: String) -> Self {
        if looks_like_context_window(&body) {
            Self::ContextWindowExceeded {
                status,
                window_tokens: parse_window_tokens(&body),
                body,
            }
        } else {
            Self::HttpError {
                status,
                retry_after_ms: None,
                transient: false,
                body,
            }
        }
    }

    /// True when this error is a context-window overflow.
    pub fn is_context_window(&self) -> bool {
        matches!(self, Self::ContextWindowExceeded { .. })
    }

    /// Mark this error as transient: safe to retry even when the HTTP
    /// status is 200 (e.g. OpenAI SSE `server_error` / `overloaded_error`
    /// mid-stream error events).
    pub fn mark_transient(mut self) -> Self {
        if let Self::HttpError {
            transient: ref mut t,
            ..
        } = self
        {
            *t = true;
        }
        self
    }

    /// Whether this error is transient and safe to retry.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::HttpError {
                transient: true,
                ..
            }
        )
    }
}

/// Case-insensitive body markers shared by the major providers (and the
/// OpenAI-compatible proxies) for context-window overflow responses.
fn looks_like_context_window(body: &str) -> bool {
    let lower = body.to_lowercase();
    const MARKERS: &[&str] = &[
        "context length",
        "maximum context",
        "context window",
        "context_window",
        "prompt is too long",
        "too many tokens",
        "token limit",
        "max_input_tokens",
        "max input tokens",
        "maximum input",
        "input token limit",
        "exceeds the maximum",
        "content is too long",
        "reduce the length",
        "reduce the amount",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Parse the model's context-window size from an error body.
///
/// Heuristic: the SMALLEST token-sized number (>= 1000) near a size keyword
/// ("maximum", "limit", "window", "context", "max") is the window — the
/// requested/used count is usually larger and/or farther away (OpenAI:
/// "maximum context length is 128000 ... resulted in 150000"; Anthropic:
/// "18000 tokens > 16000 maximum"). Falls back to the largest number in the
/// whole body when no keyword is present. Used only as a local-elimination
/// hint, never as a hard contract.
fn parse_window_tokens(body: &str) -> Option<usize> {
    let lower = body.to_lowercase();
    let bytes = lower.as_bytes();
    let numbers_in = |range: std::ops::Range<usize>| -> Vec<usize> {
        let mut out = Vec::new();
        let mut i = range.start.min(bytes.len());
        while i < range.end.min(bytes.len()) {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < range.end.min(bytes.len()) && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if let Ok(n) = lower[start..i].parse::<usize>()
                    && n >= 1000
                {
                    out.push(n);
                }
            } else {
                i += 1;
            }
        }
        out
    };

    const KEYWORDS: &[&str] = &["maximum", "limit", "window", "context", "max input", "max"];
    let mut candidates: Vec<usize> = Vec::new();
    for kw in KEYWORDS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(kw) {
            let abs = from + rel;
            let lo = abs.saturating_sub(60);
            let hi = (abs + kw.len() + 60).min(bytes.len());
            candidates.extend(numbers_in(lo..hi));
            from = abs + kw.len();
        }
    }
    if let Some(min) = candidates.iter().min() {
        return Some(*min);
    }
    numbers_in(0..bytes.len()).into_iter().max()
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider(p) => write!(f, "Unknown provider: {p}"),
            Self::MissingApiKey(p) => write!(f, "API key not set for provider: {p}"),
            Self::AnonymousModelBlocked(m) => write!(
                f,
                "Model '{m}' needs an OpenCode Zen API key — without one only the free models are served"
            ),
            Self::HttpError { status, body, .. } => write!(f, "HTTP {status} - {body}"),
            Self::ContextWindowExceeded { status, body, .. } => {
                write!(f, "HTTP {status} - {body}")
            }
            Self::Deserialization(e) => write!(f, "Failed to parse response: {e}"),
            Self::Network(e) => write!(f, "Network error: {e}"),
            Self::NoChoices => write!(f, "No choices in response"),
            Self::NoContent => write!(f, "No content in response"),
            Self::NoEmbeddings => write!(f, "No embeddings in response"),
            Self::NotImplemented(feature) => write!(f, "{feature} is not yet implemented"),
            Self::StreamTerminated => write!(f, "Stream terminated unexpectedly"),
        }
    }
}

impl std::error::Error for ConnectorError {}

impl From<reqwest::Error> for ConnectorError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

impl From<serde_json::Error> for ConnectorError {
    fn from(e: serde_json::Error) -> Self {
        Self::Deserialization(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_context_window_bodies() {
        // OpenAI
        assert!(ConnectorError::classify_http(400, "This model's maximum context length is 128000 tokens. However, your messages resulted in 150000 tokens.".into()).is_context_window());
        // Anthropic
        assert!(
            ConnectorError::classify_http(
                400,
                "prompt is too long: 18000 tokens > 16000 maximum".into()
            )
            .is_context_window()
        );
        // Gemini
        assert!(
            ConnectorError::classify_http(
                400,
                "The input is 200000 tokens long which exceeds the max input tokens of 1048576."
                    .into()
            )
            .is_context_window()
        );
        // 413 payload-too-large with a context marker
        assert!(
            ConnectorError::classify_http(
                413,
                "request body too large: exceeds the maximum context length".into()
            )
            .is_context_window()
        );
        // API-error frames arrive with status 200 — the BODY is the signal
        assert!(
            ConnectorError::classify_http(
                200,
                "This model's maximum context length is 64000 tokens.".into()
            )
            .is_context_window()
        );
    }

    #[test]
    fn does_not_classify_unrelated_errors() {
        // Plain 400s that must stay HttpError: bad JSON, invalid tool args, auth…
        assert!(
            !ConnectorError::classify_http(400, "Invalid JSON in request body".into())
                .is_context_window()
        );
        assert!(
            !ConnectorError::classify_http(400, "messages.0: tool_call_id does not match".into())
                .is_context_window()
        );
        assert!(!ConnectorError::classify_http(401, "invalid api key".into()).is_context_window());
        assert!(
            !ConnectorError::classify_http(500, "internal server error".into()).is_context_window()
        );
        assert!(
            !ConnectorError::classify_http(429, "rate limit exceeded".into()).is_context_window()
        );
    }

    #[test]
    fn parses_window_tokens_from_bodies() {
        let openai = ConnectorError::classify_http(
            400,
            "This model's maximum context length is 128000 tokens. However, your messages resulted in 150000 tokens.".into(),
        );
        let ConnectorError::ContextWindowExceeded { window_tokens, .. } = openai else {
            panic!("expected ContextWindowExceeded");
        };
        assert_eq!(window_tokens, Some(128000));

        let anthropic = ConnectorError::classify_http(
            400,
            "prompt is too long: 18000 tokens > 16000 maximum".into(),
        );
        let ConnectorError::ContextWindowExceeded { window_tokens, .. } = anthropic else {
            panic!("expected ContextWindowExceeded");
        };
        assert_eq!(window_tokens, Some(16000));

        // No size keyword near the number → falls back to the largest
        // token-sized number in the body.
        let fallback =
            ConnectorError::classify_http(400, "prompt is too long: 250000 tokens".into());
        let ConnectorError::ContextWindowExceeded { window_tokens, .. } = fallback else {
            panic!("expected ContextWindowExceeded");
        };
        assert_eq!(window_tokens, Some(250000));
    }
}
