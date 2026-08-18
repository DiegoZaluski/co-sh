//! Request retry middleware, mirroring the crush/fantasy semantics.
//!
//! Retryable failures — rate limits (429), HTTP 5xx, and every
//! network/transport-level error (connection refused, timeouts, reset
//! streams) — are retried with exponential backoff (5s → 10s → 20s),
//! honoring the server's `retry-after` / `retry-after-ms` headers when they
//! carry a sane hint. Deterministic failures (auth, context-window
//! overflow, malformed responses) are never retried.
//!
//! When a retry fires AFTER partial content was already streamed, a
//! [`StreamChunk::reset`] marker is emitted first so the consumer discards
//! the failed attempt's buffered text — the retried response restarts from
//! the beginning and must not concatenate with the partial content.

use super::error::ConnectorError;
use std::time::Duration;

/// Maximum number of RETRIES for a single request (crush/fantasy:
/// `MaxRetries = 3`) — i.e. up to 4 total attempts.
pub const RETRY_MAX_RETRIES: usize = 3;

/// Initial backoff before the first retry (crush/fantasy: 5s).
pub const RETRY_INITIAL_DELAY: Duration = Duration::from_secs(5);

/// Backoff multiplier per retry (crush/fantasy: 2.0).
pub const RETRY_BACKOFF_FACTOR: f64 = 2.0;

/// Upper bound for a server-provided retry hint (crush/fantasy: 60s).
const RETRY_HINT_MAX: Duration = Duration::from_secs(60);

/// Whether a connector error should be retried: rate limits (429), HTTP 5xx,
/// and every network/transport-level failure (connection refused, timeouts,
/// reset streams). Deterministic failures — auth (401/403), context-window
/// overflow, malformed responses — are NOT retried: re-running them would
/// only waste time and tokens.
#[must_use]
pub fn is_retryable_error(e: &ConnectorError) -> bool {
    match e {
        ConnectorError::Network(_) | ConnectorError::StreamTerminated => true,
        ConnectorError::HttpError {
            status, transient, ..
        } => *transient || *status == 429 || (500..=599).contains(status),
        _ => false,
    }
}

/// Backoff delay before retry `attempt` (1-based): exponential by default
/// (`initial * factor^(attempt-1)`), overridden by the server's retry hint
/// when one was attached to the error and it is smaller and within the sane
/// bounds (mirrors fantasy's `getRetryDelayInMs`).
#[must_use]
pub fn retry_delay(e: &ConnectorError, attempt: usize) -> Duration {
    let base =
        RETRY_INITIAL_DELAY.mul_f64(RETRY_BACKOFF_FACTOR.powi(attempt.saturating_sub(1) as i32));
    if let ConnectorError::HttpError {
        retry_after_ms: Some(ms),
        ..
    } = e
    {
        let hinted = Duration::from_millis(*ms);
        if hinted < RETRY_HINT_MAX && hinted < base {
            return hinted;
        }
    }
    base
}
