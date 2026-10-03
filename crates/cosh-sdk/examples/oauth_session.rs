//! TokenSession end-to-end — fully offline, no provider needed.
//!
//! Run it:
//!
//! ```sh
//! cargo run -p cosh-sdk --example oauth_session
//! ```
//!
//! Demonstrates the session API a consumer drives after the initial
//! authorization: automatic renewal, proactive 401 handling, consumer-owned
//! persistence (`snapshot` + `TokenSet::from_stored`), and renewal
//! observability via `subscribe`. A stub [`TokenRefresher`] stands in for the
//! network so the example runs offline; in a real application use
//! `StandardRefresher` (same token endpoint as the flow) or, for Charm
//! Hyper, the `CharmRefresher` adapter.

use cosh_sdk::connector::oauth::{
    OAuthProviderConfig, Secret, TokenRefresher, TokenSession, TokenSet,
};
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};
use url::Url;

/// Offline stand-in for the production refresher: mints a new token set
/// without touching the network. Real implementations POST the refresh token
/// to the provider's token endpoint over HTTPS (or, for Charm Hyper, use the
/// `CharmRefresher` adapter from `openai_compatible`).
struct StubRefresher {
    tag: String,
}

impl TokenRefresher for StubRefresher {
    fn refresh<'a>(
        &'a self,
        _refresh_token: &'a Secret,
    ) -> Pin<Box<dyn Future<Output = cosh_sdk::connector::oauth::Result<TokenSet>> + Send + 'a>>
    {
        Box::pin(async move {
            // Real implementations preserve rotation semantics: a missing
            // refresh token in the response keeps the current one
            // (TokenSet::inherit_refresh, applied by TokenSession).
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default();
            Ok(TokenSet::new(
                Secret::new(format!("access-{}-{now}", self.tag)),
                Some(Secret::new(format!("refresh-{}", self.tag))),
                Some(SystemTime::now() + Duration::from_secs(3600)),
                vec!["read".into(), "write".into()],
            ))
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The config validates the endpoint invariants a real refresher needs;
    // it is not contacted by this example.
    let config = OAuthProviderConfig::new(
        Url::parse("https://provider.example/oauth/authorize")?,
        Url::parse("https://provider.example/oauth/token")?,
        "example-client-id",
    );
    config.validate()?;

    // A token set as the authorization flow would have produced it.
    let tokens = TokenSet::new(
        Secret::new("access-token-from-flow"),
        Some(Secret::new("refresh-token-from-flow")),
        Some(SystemTime::now() + Duration::from_secs(3600)),
        vec!["read".into(), "write".into()],
    );
    let session = TokenSession::new(tokens, Box::new(StubRefresher { tag: "stub".into() }));

    // Always-current access token: renewals are single-flight, so any number
    // of tasks can call this concurrently.
    let access = session.valid_access_token().await?;
    println!("1. valid_access_token -> {}…", &access.expose()[..18]);

    // The resource server rejected the token (proactive 401): force a renewal
    // supplying the rejected token, so a stale late 401 for an older token
    // cannot trigger a spurious refresh.
    let fresh = session.refresh_after_rejection(&access).await?;
    println!("2. refresh_after_rejection -> {}…", &fresh.expose()[..18]);

    // Persistence is consumer-owned. Snapshot now, store it wherever it is
    // safe in your application (keyring, encrypted store); restore it later
    // with TokenSet::from_stored — which rejects expired snapshots.
    let stored = session.snapshot();
    println!("3. snapshot (as stored by the consumer): {stored:?}");
    let restored = TokenSet::from_stored(&stored)?;
    println!(
        "4. from_stored -> valid: {}, expired: {}",
        restored.access_token_valid(Duration::from_secs(30)),
        restored.is_expired()
    );

    // Renewals are observable independently of the request path: persist the
    // latest snapshot on every notification so a crash never loses a rotated
    // refresh token.
    let mut renewals = session.subscribe();
    renewals.borrow_and_update();
    println!("5. subscribe -> receiver ready; snapshot() is the pull path");

    println!("6. expires_at: {:?}", session.expires_at());
    Ok(())
}
