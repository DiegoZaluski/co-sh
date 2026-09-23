//! Crate-internal transport tests: the sender, the hardened
//! client and the [`crate::telemetry::sink::Consent`] token are `pub(crate)`,
//! so the network-level regression cases live INSIDE the crate. Each test
//! asserts the SAFE outcome required by the original audit (F03, F08) and the
//! re-audit (C02, C03); `control_*` tests verify a remediation stays in place.
//!
//! The TLS fixture is a fully in-process Rust server (rcgen certificate +
//! tokio-rustls): no external process, no Python, no openssl CLI. Loopback
//! only; the throwaway certificate is explicitly trusted by the test client
//! and never installed globally.

use crate::telemetry::Telemetry;
use crate::telemetry::events::synthetic;
use crate::telemetry::queue::EventQueue;
use crate::telemetry::schema::EventType;
use crate::telemetry::sink::{Consent, FlushOutcome, SinkConfig, client, flush};
use serde_json::{Value, json};
use std::{
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    _root: tempfile::TempDir,
    endpoint: String,
    cert_der: rustls::pki_types::CertificateDer<'static>,
    receipts: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    /// Start the loopback TLS ingest. `responses` is a list of response specs
    /// consumed in request order (the last one repeats when exhausted):
    ///   {"status": 503}                                     -> empty body
    ///   {"status": 429, "headers": {"Retry-After": "..."}}  -> extra headers
    ///   {"status": 307, "redirect_plain": true}             -> Location: http://...
    /// Every received request appends {"transport": "tls", "body": ...} to
    /// the in-memory receipt list.
    async fn start(responses: Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let receipts: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let specs: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(
            responses.as_array().cloned().unwrap_or_default(),
        ));
        let next: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

        // Throwaway self-signed certificate (loopback only): trusted by the
        // test client via `add_root_certificate`, never installed globally.
        let certified_key =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])
                .expect("rcgen key generation is infallible for these inputs");
        let cert_der = certified_key.cert.der().clone();
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified_key.key_pair.serialize_der()),
        );
        let server_config = Arc::new(
            // Explicit provider: reqwest pulls aws-lc-rs into the test binary,
            // so rustls cannot auto-select — pin ring (what our dev-dep uses).
            rustls::ServerConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)
            .expect("valid self-signed certificate"),
        );
        let acceptor = Arc::new(tokio_rustls::TlsAcceptor::from(server_config));

        let (listener, endpoint) = {
            // Bind on the CURRENT runtime (the #[tokio::test] one) so the
            // port is known before spawning the accept loop.
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("loopback bind cannot fail in tests");
            let endpoint = format!(
                "https://127.0.0.1:{}/ingest",
                listener.local_addr().unwrap().port()
            );
            (listener, endpoint)
        };

        let receipts_task = receipts.clone();
        let server = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let specs = specs.clone();
                let next = next.clone();
                let receipts = receipts_task.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    // Read the HTTP request: headers, then Content-Length body.
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        match tls.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                                    break pos + 4;
                                }
                            }
                        }
                    };
                    let headers = String::from_utf8_lossy(&buf[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    while buf.len() < header_end + content_length {
                        match tls.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body =
                        String::from_utf8_lossy(&buf[header_end..header_end + content_length])
                            .to_string();

                    let spec = {
                        let mut idx = next.lock().unwrap();
                        let list = specs.lock().unwrap();
                        let spec = list[(*idx).min(list.len().saturating_sub(1))].clone();
                        *idx += 1;
                        spec
                    };
                    receipts.lock().unwrap().push(json!({
                        "transport": "tls",
                        "body": body,
                    }));

                    let status = spec.get("status").and_then(Value::as_u64).unwrap_or(200);
                    let reason = match status {
                        204 => "No Content",
                        307 => "Temporary Redirect",
                        429 => "Too Many Requests",
                        500 => "Internal Server Error",
                        502 => "Bad Gateway",
                        503 => "Service Unavailable",
                        504 => "Gateway Timeout",
                        _ => "OK",
                    };
                    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
                    if let Some(extra) = spec.get("headers").and_then(Value::as_object) {
                        for (name, value) in extra {
                            if let Some(v) = value.as_str() {
                                response.push_str(&format!("{name}: {v}\r\n"));
                            }
                        }
                    }
                    if spec
                        .get("redirect_plain")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        response.push_str("Location: http://127.0.0.1:1/ingest\r\n");
                    }
                    response.push_str("Content-Length: 0\r\n\r\n");
                    let _ = tls.write_all(response.as_bytes()).await;
                    let _ = tls.flush().await;
                    let _ = tls.shutdown().await;
                });
            }
        });

        Self {
            _root: root,
            endpoint,
            cert_der,
            receipts,
            server,
        }
    }

    /// The production client configuration (https_only, no redirects, no
    /// proxy — identical hardening to `sink::client()`) PLUS the fixture's
    /// throwaway certificate, which is the ONLY way a loopback TLS server can
    /// be reached. The hardening under test (redirect refusal, https-only) is
    /// exactly what production uses.
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .add_root_certificate(
                reqwest::Certificate::from_der(self.cert_der.as_ref())
                    .expect("valid DER certificate"),
            )
            .build()
            .unwrap()
    }

    fn config(&self, attempts: u32) -> SinkConfig {
        SinkConfig {
            endpoint: self.endpoint.clone(),
            publishable_key: "sb_publishable_synthetic_transport".into(),
            max_attempts: attempts,
        }
    }

    fn queue(&self) -> EventQueue {
        EventQueue::new(self._root.path().join("queue"))
    }

    fn receipts(&self) -> Vec<Value> {
        self.receipts.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn control_actual_5xx_responses_preserve_events() {
    // 500/502/503/504 are TRANSIENT — the drained batch is
    // re-queued with identical content, never dropped.
    for status in [500, 502, 503, 504] {
        let fixture = Fixture::start(json!([{"status": status}])).await;
        let queue = fixture.queue();
        let event = synthetic(EventType::Error);
        queue.push(&event);
        assert_eq!(
            flush(
                &queue,
                &fixture.config(1),
                &fixture.client(),
                Consent::granted()
            )
            .await,
            FlushOutcome::Failed
        );
        assert_eq!(fixture.receipts().len(), 1);
        assert_eq!(queue.pending_count(), 1);
        assert_eq!(
            queue.drain_batch()[0].client_event_id(),
            event.client_event_id()
        );
    }
}

#[tokio::test]
async fn control_503_retry_sends_identical_event_then_succeeds() {
    // a transient response must not corrupt or lose the event;
    // the retransmission is byte-identical.
    let fixture = Fixture::start(json!([{"status": 503}, {"status": 204}])).await;
    let queue = fixture.queue();
    queue.push(&synthetic(EventType::Error));
    assert_eq!(
        flush(
            &queue,
            &fixture.config(2),
            &fixture.client(),
            Consent::granted()
        )
        .await,
        FlushOutcome::Sent(1)
    );
    let receipts = fixture.receipts();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0]["body"], receipts[1]["body"]);
    assert_eq!(queue.pending_count(), 0);
}

#[tokio::test]
async fn retry_after_http_date_is_honored() {
    // RFC 9110 §10.2.3: the HTTP-date form of `Retry-After`
    // must be parsed and honored. The date is +2s (kept short so the flush
    // still finishes fast); the safe outcome is a completed second attempt.
    let later = (chrono::Utc::now() + chrono::Duration::seconds(2))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let fixture = Fixture::start(json!([
        {"status": 429, "headers": {"Retry-After": later}},
        {"status": 204}
    ]))
    .await;
    let queue = fixture.queue();
    queue.push(&synthetic(EventType::Error));
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        flush(
            &queue,
            &fixture.config(2),
            &fixture.client(),
            Consent::granted(),
        ),
    )
    .await
    .expect("flush must finish well inside the timeout");
    assert_eq!(outcome, FlushOutcome::Sent(1));
    assert_eq!(fixture.receipts().len(), 2);
}

#[tokio::test]
async fn https_redirect_to_plain_http_is_refused() {
    // Safe outcome: with the hardened client, a 307 redirect to
    // plain HTTP is NEVER followed — no byte of the batch reaches the second
    // origin. The unhandled 3xx is classified transient and re-queued.
    let fixture = Fixture::start(json!([{"status": 307, "redirect_plain": true}])).await;
    let queue = fixture.queue();
    queue.push(&synthetic(EventType::Error));
    let config = fixture.config(1);
    assert!(config.validate(), "initial endpoint really is HTTPS");
    assert_eq!(
        flush(&queue, &config, &fixture.client(), Consent::granted()).await,
        FlushOutcome::Failed,
        "an unfollowed redirect must NOT count as delivered"
    );
    let receipts = fixture.receipts();
    assert_eq!(
        receipts.len(),
        1,
        "exactly one hop: the redirect is not followed"
    );
    assert_eq!(receipts[0]["transport"], "tls");
    assert_eq!(
        queue.pending_count(),
        1,
        "the event stays queued for the real endpoint"
    );
}

#[tokio::test]
async fn transport_client_refuses_plain_http_urls() {
    // the production client itself cannot be pointed at http://.
    let cl = client();
    let response = cl.get("http://127.0.0.1:1/ingest").send().await;
    assert!(response.is_err(), "https_only client must refuse http URLs");
}

#[tokio::test]
async fn invalid_endpoint_short_circuits_to_idle() {
    // endpoint validation happens BEFORE any request or drain.
    let dir = tempfile::tempdir().unwrap();
    let queue = EventQueue::new(dir.path());
    queue.push(&synthetic(EventType::Error));
    let config = SinkConfig {
        endpoint: "http://127.0.0.1:1/ingest".into(),
        publishable_key: "sb_publishable_synthetic_transport".into(),
        max_attempts: 1,
    };
    assert!(!config.validate(), "http endpoints must be rejected");
    assert_eq!(
        flush(&queue, &config, &client(), Consent::granted()).await,
        FlushOutcome::Idle
    );
    assert_eq!(queue.pending_count(), 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn consent_cannot_be_overridden_by_a_caller() {
    // Safe outcome: a disabled facade flushes to Idle and never
    // drains the queue. The remaining override path is closed at COMPILE
    // time: `sink::flush` requires a `Consent` token whose only constructor
    // is `pub(crate)` (called exclusively by the enabled facade), so no
    // external caller can pass a hand-made `true` anymore.
    const NAME: &str = "telemetry::transport_tests::consent_cannot_be_overridden_by_a_caller";
    if std::env::var("COSH_TRANSPORT_CHILD").as_deref() != Ok(NAME) {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env("COSH_TRANSPORT_CHILD", NAME)
            .env("COSH_TELEMETRY", "off")
            .env("CI", "1")
            .env("XDG_DATA_HOME", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let telemetry = Telemetry::resolve(false);
    assert!(!telemetry.enabled());
    assert!(
        telemetry
            .queue()
            .path()
            .starts_with(std::env::var("XDG_DATA_HOME").unwrap())
    );
    telemetry.queue().push(&synthetic(EventType::Error));
    let fixture = Fixture::start(json!([{"status": 204}])).await;
    let config = fixture.config(1);
    assert_eq!(telemetry.flush(&config).await, FlushOutcome::Idle);
    assert!(fixture.receipts().is_empty());
    assert_eq!(
        telemetry.queue().pending_count(),
        1,
        "disabled flush must not consume the queue"
    );
}
