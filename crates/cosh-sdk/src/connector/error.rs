use std::fmt;

/// Errors returned by the [`Connector`](crate::connector::Connector) API.
#[derive(Debug)]
pub enum ConnectorError {
    /// The provider name passed to [`Connector::new`](crate::connector::Connector::new) is not recognized.
    UnknownProvider(String),
    /// No API key was found for the provider (neither explicit nor in env vars).
    MissingApiKey(String),
    /// The upstream API returned a non-2xx HTTP status.
    HttpError {
        /// HTTP status code (e.g., 401, 500).
        status: u16,
        /// Response body from the server.
        body: String,
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

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider(p) => write!(f, "Unknown provider: {p}"),
            Self::MissingApiKey(p) => write!(f, "API key not set for provider: {p}"),
            Self::HttpError { status, body } => write!(f, "HTTP {status} - {body}"),
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
