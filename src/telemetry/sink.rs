//! Telemetry sink: uploads queued events to the ingest endpoint.
//!
//! Security model (plan §3.3):
//!
//! - The client NEVER holds a secret key. It POSTs batches to the
//!   edge-function ingest endpoint using only the project URL + publishable
//!   key; the function validates payloads and inserts with the secret key
//!   server-side.
//! - Transport is pinned down: the dedicated client requires
//!   HTTPS, follows NO redirects, and never uses machine proxy settings. A
//!   redirected or downgraded endpoint fails instead of leaking the batch.
//! - The sink re-validates every persisted envelope before sending (audit
//!   F02): queue content is untrusted data.
//! - HTTP classification: only 4xx-except-429/503 is permanent;
//!   429 and 5xx are transient and honor the server's `Retry-After`.

use std::time::Duration;

use crate::telemetry::events::EventEnvelope;
use crate::telemetry::queue::{EventQueue, MAX_BATCH};

/// Ingest configuration, resolved from env/config at runtime.
#[derive(Debug, Clone)]
pub struct SinkConfig {
    /// Full ingest endpoint (edge function URL), e.g.
    /// `https://<ref>.supabase.co/functions/v1/telemetry-ingest`.
    pub endpoint: String,
    /// Publishable key (`sb_publishable_...`). Safe to ship in a CLI binary:
    /// it only reaches what RLS/API grants allow (public SELECT, no INSERT).
    pub publishable_key: String,
    /// Max retry attempts per batch.
    pub max_attempts: u32,
}

impl SinkConfig {
    /// Deployment config from the environment: `COSH_TELEMETRY_ENDPOINT`
    /// (edge-function URL) and `COSH_TELEMETRY_PUBLISHABLE_KEY`. `None` when
    /// either is unset or blank: a build without ingest configuration never
    /// sends anything (the queue persists locally for a later flush).
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("COSH_TELEMETRY_ENDPOINT").ok()?;
        let publishable_key = std::env::var("COSH_TELEMETRY_PUBLISHABLE_KEY").ok()?;
        let endpoint = endpoint.trim().to_string();
        let publishable_key = publishable_key.trim().to_string();
        if endpoint.is_empty() || publishable_key.is_empty() {
            return None;
        }
        Some(Self {
            endpoint,
            publishable_key,
            max_attempts: 3,
        })
    }

    /// Validate the endpoint: production ingest MUST be https.
    /// Returns `false` for http://, empty or malformed values.
    pub fn validate(&self) -> bool {
        self.endpoint.starts_with("https://")
            && url::Url::parse(&self.endpoint)
                .map(|u| u.scheme() == "https" && !u.host_str().unwrap_or_default().is_empty())
                .unwrap_or(false)
            && !self.publishable_key.is_empty()
            && self.max_attempts > 0
    }
}

/// Outcome of a flush attempt.
#[derive(Debug, PartialEq)]
pub enum FlushOutcome {
    /// Telemetry disabled or nothing to send.
    Idle,
    /// Batch accepted by the ingest.
    Sent(usize),
    /// Ingest rejected the batch permanently (4xx, non-429): the batch is
    /// dropped, never re-queued (a bad payload stays bad).
    Rejected,
    /// Transient failure after retries; events re-queued for the next flush.
    Failed,
}

/// Backoff schedule for retries (ms). Capped so a dead endpoint doesn't keep
/// a tokio task alive forever.
fn backoff_ms(attempt: u32) -> u64 {
    let base = 500u64.saturating_mul(1 << attempt.min(6));
    base.min(30_000)
}

/// Hard cap on a server-requested Retry-After wait (seconds). A malicious or
/// buggy server cannot pin a flush task for more than this.
const MAX_RETRY_AFTER_SECS: u64 = 300;

/// Parse a `Retry-After` header (RFC 9110 §10.2.3). BOTH standardized forms
/// are honored: delta-seconds and HTTP-date. Values above
/// [`MAX_RETRY_AFTER_SECS`] are CLAMPED to the cap, not discarded: the
/// server's intent (wait longer) must win over the local backoff.
fn retry_after_secs(header: &str) -> Option<Duration> {
    let header = header.trim();
    // Form 1: delta-seconds.
    if let Ok(secs) = header.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS)));
    }
    // Form 2: HTTP-date (IMF-fixdate, the only form modern servers emit;
    // the legacy RFC 850 / asctime forms are not worth the parse risk).
    // NaiveDateTime + and_utc: the wire format carries no numeric offset, and
    // DateTime::parse_from_str REQUIRES one — it would reject every header.
    // The literal " GMT" must stay in the pattern: parse consumes the WHOLE
    // string, and the trailing marker would otherwise fail the parse.
    let target = chrono::NaiveDateTime::parse_from_str(header, "%a, %d %b %Y %H:%M:%S GMT")
        .ok()?
        .and_utc();
    let wait = target.signed_duration_since(chrono::Utc::now());
    if wait <= chrono::Duration::zero() {
        return Some(Duration::ZERO); // date already past: no deferral left
    }
    Some(Duration::from_secs(
        (wait.num_seconds() as u64).min(MAX_RETRY_AFTER_SECS),
    ))
}

/// Dedicated HTTP client for telemetry: HTTPS-only, redirects
/// disabled, machine proxies ignored.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("telemetry client construction is infallible by configuration")
}

/// Unforgeable consent token. The sender can no longer be
/// called with a hand-made `true`: the ONLY constructor is `Consent::granted`,
/// which is `pub(crate)` and is called exclusively by
/// [`crate::telemetry::Telemetry::flush`] after the facade's consent
/// resolution. Zero-sized; the private field makes literal construction
/// impossible outside this crate.
#[derive(Debug, Clone, Copy)]
pub struct Consent {
    _sealed: (),
}

impl Consent {
    pub(crate) fn granted() -> Self {
        Self { _sealed: () }
    }
}

/// Flush up to `MAX_BATCH` events from the queue. Crate-internal (reaudit
/// C02): the ONLY supported transmission path is the facade method
/// [`crate::telemetry::Telemetry::flush`], which supplies both the hardened
/// client and the [`Consent`] token derived from the CURRENT consent state.
/// Synchronous, best-effort: call from a spawned tokio task (never from the
/// render loop).
pub(crate) async fn flush(
    queue: &EventQueue,
    config: &SinkConfig,
    client: &reqwest::Client,
    _consent: Consent,
) -> FlushOutcome {
    if !config.validate() {
        return FlushOutcome::Idle;
    }
    let batch = queue.drain_batch();
    if batch.is_empty() {
        return FlushOutcome::Idle;
    }
    // drain_batch never returns more than MAX_BATCH; guard the invariant so a
    // future constant change cannot silently drop the tail.
    debug_assert!(
        batch.len() <= MAX_BATCH,
        "drain_batch returned more than MAX_BATCH"
    );
    let payload = &batch[..batch.len().min(MAX_BATCH)];
    match send_batch(payload, config, client).await {
        Ok(()) => FlushOutcome::Sent(payload.len()),
        // Permanent rejection: the payload is invalid (schema changed, too
        // big). Re-queueing would poison the queue head — drop the batch.
        Err(SendError::Permanent) => FlushOutcome::Rejected,
        // Transient failure: re-queue for the next flush (the drain removed
        // them from the file, so pushing back is exactly right).
        Err(SendError::Transient) => {
            for event in payload {
                queue.push(event);
            }
            FlushOutcome::Failed
        }
    }
}

/// Why a batch send failed.
#[derive(Debug)]
enum SendError {
    /// 4xx other than 429 — retrying cannot succeed.
    Permanent,
    /// Network error, 5xx or 429 — may succeed later.
    Transient,
}

/// POST one batch to the ingest endpoint with retries. Server failures
/// (429/5xx) are TRANSIENT: the batch is re-queued by the caller,
/// never dropped. A `Retry-After` header on 429/503 is honored.
async fn send_batch(
    batch: &[EventEnvelope],
    config: &SinkConfig,
    client: &reqwest::Client,
) -> Result<(), SendError> {
    // Re-validation at the transport boundary: persisted queue
    // content is untrusted; anything failing the invariants is dropped here
    // (fail closed) and never transmitted.
    if !batch.iter().all(EventEnvelope::validate) {
        return Err(SendError::Permanent);
    }
    let body = serde_json::to_string(batch).map_err(|_| SendError::Permanent)?;
    for attempt in 0..config.max_attempts {
        let request = client
            .post(&config.endpoint)
            .header("apikey", &config.publishable_key)
            .header("Content-Type", "application/json")
            .body(body.clone());
        let mut server_wait: Option<Duration> = None;
        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            // 429/503: transient, honor Retry-After when present.
            Ok(response) if matches!(response.status().as_u16(), 429 | 503) => {
                server_wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(retry_after_secs);
            }
            // Any other 4xx: the payload is bad — retrying won't help.
            Ok(response) if response.status().is_client_error() => {
                return Err(SendError::Permanent);
            }
            // 5xx (500, 502, 504, ...): server-side condition, transient.
            Ok(_) => {}
            // Network/timeout error: transient.
            Err(_) => {}
        }
        if attempt + 1 >= config.max_attempts {
            return Err(SendError::Transient);
        }
        let wait = server_wait.unwrap_or_else(|| Duration::from_millis(backoff_ms(attempt)));
        tokio::time::sleep(wait).await;
    }
    Err(SendError::Transient)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_capped_and_monotonic() {
        assert_eq!(backoff_ms(0), 500);
        assert!(backoff_ms(5) > backoff_ms(1));
        assert!(backoff_ms(10) <= 30_000);
    }

    #[test]
    fn endpoint_validation_requires_https() {
        let ok = |endpoint: &str| {
            SinkConfig {
                endpoint: endpoint.into(),
                publishable_key: "sb_publishable_x".into(),
                max_attempts: 1,
            }
            .validate()
        };
        assert!(ok("https://ref.supabase.co/functions/v1/telemetry-ingest"));
        assert!(!ok("http://ref.supabase.co/functions/v1/telemetry-ingest"));
        assert!(!ok(""));
        assert!(!ok("not a url"));
    }

    #[test]
    fn retry_after_is_parsed_and_capped() {
        assert_eq!(retry_after_secs("120"), Some(Duration::from_secs(120)));
        assert_eq!(retry_after_secs("0"), Some(Duration::ZERO));
        // Above the hard cap: clamped, never discarded.
        assert_eq!(retry_after_secs("99999"), Some(Duration::from_secs(300)));
        assert_eq!(retry_after_secs("garbage"), None);
        assert_eq!(retry_after_secs("-5"), None);
        // HTTP-date form (RFC 9110 §10.2.3).
        let later = (chrono::Utc::now() + chrono::Duration::seconds(120))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let secs = retry_after_secs(&later)
            .expect("HTTP-date form must be parsed")
            .as_secs();
        assert!(
            (100..=120).contains(&secs),
            "120s HTTP-date must resolve to ~120s, got {secs}"
        );
        assert_eq!(
            retry_after_secs("Mon, 01 Jan 2035 00:00:00 GMT"),
            Some(Duration::from_secs(MAX_RETRY_AFTER_SECS)),
            "far-future HTTP-date must clamp to the cap"
        );
        assert_eq!(
            retry_after_secs("Mon, 01 Jan 1990 00:00:00 GMT"),
            Some(Duration::ZERO),
            "past HTTP-date means no deferral left"
        );
    }

    #[tokio::test]
    async fn flush_is_idle_on_empty_queue_or_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let queue = EventQueue::new(dir.path());
        let config = SinkConfig {
            endpoint: "https://localhost:1/never".into(),
            publishable_key: "sb_publishable_test".into(),
            max_attempts: 1,
        };
        let cl = client();
        assert_eq!(
            flush(&queue, &config, &cl, Consent::granted()).await,
            FlushOutcome::Idle
        );
        // Invalid (non-https) endpoint → Idle, never attempted.
        let config = SinkConfig {
            endpoint: "http://localhost:1/never".into(),
            publishable_key: "sb_publishable_test".into(),
            max_attempts: 1,
        };
        assert_eq!(
            flush(&queue, &config, &cl, Consent::granted()).await,
            FlushOutcome::Idle
        );
    }
}
