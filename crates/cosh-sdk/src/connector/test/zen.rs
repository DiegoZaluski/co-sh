//! Tests for the OpenCode Zen anonymous free tier.
//!
//! Covers: sentinel fallback (`Bearer public`) after opt-in, free-model
//! enforcement for anonymous sessions (paid models fail fast WITHOUT a
//! network round-trip), account-key precedence over the sentinel,
//! anonymous `list_models` filtering, the process-wide opt-in switch, and
//! the honest `User-Agent` identification.

use super::super::{
    Connector, ZEN_PROVIDER, ZEN_PUBLIC_KEY, is_zen_free_model, set_zen_public_tier_enabled,
    zen_public_tier_enabled,
};
use super::common::{ENV_LOCK, EnvGuard, mock_server};

const CHAT_OK: &str = r#"{"choices":[{"message":{"content":"ok"}}]}"#;

/// A keyless Zen connector pointed at a local mock server, fully isolated
/// from the OS keyring AND from any `OPENCODE_API_KEY` export. Every test
/// using it must hold [`ENV_LOCK`] and create its own `EnvGuard`.
fn zen_no_key(port: u16) -> Connector {
    Connector::new(ZEN_PROVIDER)
        .unwrap()
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_service_keyring("cosh-tests-no-key")
}

/// Free models are listed by ID in the allowlist helper.
#[test]
fn free_model_allowlist_membership() {
    assert!(is_zen_free_model("big-pickle"));
    assert!(is_zen_free_model("x-preview-f-free"));
    // Deliberate exclusion: contributor tier trains on prompts/completions.
    assert!(!is_zen_free_model("muse-spark-1.2-contributor-free"));
    assert!(!is_zen_free_model("gpt-5.2"));
}

/// With opt-in and no resolvable key, a FREE model goes out carrying the
/// gateway's public sentinel — and cosh's own User-Agent.
#[tokio::test]
async fn anonymous_free_model_sends_public_sentinel() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");
    let (port, _body, raw, handle) = mock_server(CHAT_OK, 200);
    let result = zen_no_key(port)
        .with_zen_public_tier(true)
        .with_model("big-pickle")
        .chat("hello")
        .await;
    handle.join().unwrap();
    assert!(result.is_ok());

    let req = raw.lock().unwrap().take().unwrap();
    assert!(
        req.to_lowercase()
            .contains(&format!("authorization: bearer {ZEN_PUBLIC_KEY}")),
        "public sentinel missing from request"
    );
    assert!(
        req.to_lowercase().contains("user-agent: cosh/"),
        "co-sh User-Agent missing from request"
    );
}

/// An anonymous request for a PAID model fails fast — before any network
/// activity — with guidance to connect a key.
#[tokio::test]
async fn anonymous_paid_model_blocked_before_network() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");
    // Unreachable port: if the request escaped client-side enforcement it
    // would surface as a Network error instead of the dedicated variant.
    let err = zen_no_key(1)
        .with_zen_public_tier(true)
        .with_model("gpt-5.2")
        .chat("hello")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("needs an OpenCode Zen API key"),
        "dedicated blocked-model message expected, got: {msg}"
    );
    assert!(msg.contains("gpt-5.2"), "model id missing from: {msg}");
}

/// Without the opt-in there is NO sentinel fallback: the plain missing-key
/// error is returned (this is what keeps anonymous usage strictly opt-in).
#[tokio::test]
async fn no_opt_in_still_requires_key() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");
    let err = zen_no_key(1)
        .with_model("big-pickle")
        .chat("hello")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("API key not set for provider: opencode"),
        "got: {err}"
    );
}

/// A real account key ALWAYS wins over the anonymous sentinel — paid models
/// go out under the user's own credential.
#[tokio::test]
async fn account_key_wins_over_sentinel() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");
    let (port, _body, raw, handle) = mock_server(CHAT_OK, 200);
    let result = zen_no_key(port)
        .with_zen_public_tier(true)
        .with_api_key("sk-zen-account")
        .with_model("gpt-5.2")
        .chat("hello")
        .await;
    handle.join().unwrap();
    assert!(result.is_ok(), "keyed session must reach the server");

    let req = raw.lock().unwrap().take().unwrap();
    assert!(
        req.to_lowercase()
            .contains("authorization: bearer sk-zen-account"),
        "account key missing from request"
    );
    assert!(
        !req.to_lowercase()
            .contains(&format!("bearer {ZEN_PUBLIC_KEY}")),
        "sentinel must not appear alongside an account key"
    );
}

/// Anonymous `list_models` only surfaces the free tier; a keyed session sees
/// everything the gateway reports.
#[tokio::test]
async fn list_models_filtered_when_anonymous() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");
    let body = r#"{"data":[{"id":"big-pickle"},{"id":"gpt-5.2"},{"id":"x-preview-f-free"},{"id":"claude-sonnet-4-6"}]}"#;
    let (port, _req_body, _raw, handle) = mock_server(body, 200);
    let anonymous = zen_no_key(port)
        .with_zen_public_tier(true)
        .list_models()
        .await;
    handle.join().unwrap();
    let ids: Vec<String> = anonymous
        .unwrap()
        .models()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(ids, vec!["big-pickle", "x-preview-f-free"]);

    let (port, _req_body, _raw, handle) = mock_server(body, 200);
    let keyed = zen_no_key(port)
        .with_api_key("sk-zen-account")
        .list_models()
        .await;
    handle.join().unwrap();
    assert_eq!(keyed.unwrap().models().len(), 4);
}

/// The process-wide switch enables the sentinel for connectors built WITHOUT
/// the per-connector flag — how harness-internal constructions inherit the
/// user's persisted opt-in.
#[tokio::test]
async fn global_switch_enables_public_tier() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");

    set_zen_public_tier_enabled(false);
    assert!(!zen_public_tier_enabled());
    let err = zen_no_key(1)
        .with_model("big-pickle")
        .chat("hello")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("API key not set"));

    set_zen_public_tier_enabled(true);
    let (port, _body, raw, handle) = mock_server(CHAT_OK, 200);
    let result = zen_no_key(port)
        .with_model("big-pickle")
        .chat("hello")
        .await;
    handle.join().unwrap();
    assert!(result.is_ok());
    assert!(
        raw.lock()
            .unwrap()
            .take()
            .unwrap()
            .to_lowercase()
            .contains(&format!("bearer {ZEN_PUBLIC_KEY}"))
    );

    // Restore the default so unrelated tests never see the opt-in on.
    set_zen_public_tier_enabled(false);
}

/// `is_anonymous` mirrors exactly when the sentinel will be sent.
#[tokio::test]
async fn is_anonymous_reflects_resolution() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENCODE_API_KEY");

    let c = zen_no_key(1).with_zen_public_tier(true);
    assert!(c.is_anonymous());

    assert!(!c.clone().with_api_key("sk-zen-account").is_anonymous());
    assert!(
        !Connector::new("openai")
            .unwrap()
            .with_base_url("http://127.0.0.1:1/v1")
            .with_api_key("sk-test")
            .is_anonymous()
    );
}
