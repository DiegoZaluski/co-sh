//! OAuth 2.0 Authorization Code + PKCE against a provider of your choice.
//!
//! Run it with the endpoints and client id as arguments:
//!
//! ```sh
//! cargo run -p cosh-sdk --example oauth_authorize -- \
//!     https://provider.example/oauth/authorize \
//!     https://provider.example/oauth/token \
//!     my-client-id
//! ```
//!
//! The example starts a loopback listener (RFC 8252), opens the system
//! browser, waits for the redirect, validates `state` and — when the provider
//! sends it — the RFC 9207 `iss` parameter, exchanges the code, and then runs
//! a short session demonstrating automatic refresh and consumer-owned
//! persistence.
//!
//! SECURITY: the transaction URL printed below carries `state` and, later,
//! the code. This is fine for an interactive terminal; never pipe it to logs
//! in a real application.

use cosh_sdk::connector::oauth::{OAuthProviderConfig, StandardRefresher, TokenSession, authorize};
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(authorization), Some(token), Some(client_id), ..) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: cargo run -p cosh-sdk --example oauth_authorize -- \
             <authorization-url> <token-url> <client-id>"
        );
        std::process::exit(2);
    };

    let mut config =
        OAuthProviderConfig::new(Url::parse(&authorization)?, Url::parse(&token)?, client_id);
    // Fields are public by design: provider quirks stay data, not code.
    config.scopes = vec!["read".into(), "write".into()];
    // RFC 9207 mix-up defense: set when the provider sends `iss`.
    config.expected_issuer = Some(Url::parse(&authorization)?.origin().ascii_serialization());
    // Always validate before use: enforces HTTPS on remote endpoints, a
    // non-empty client id, and a safe callback path.
    config.validate()?;

    // One call: bind the loopback receiver, generate PKCE S256 + state, open
    // the browser, wait for the callback, exchange the code.
    let tokens = authorize(&config, true, |url| {
        println!("Open this URL in your browser (also opening automatically):\n  {url}");
    })
    .await?;

    println!("Authorized. Scopes: {:?}", tokens.scopes());

    // Wrap the token set in a session: renewal is single-flight and a refresh
    // with unknown outcome latches the session instead of replaying a
    // possibly-rotated token (RFC 9700 §4.14.2).
    let session = TokenSession::new(tokens, Box::new(StandardRefresher::new(config)?));

    let access = session.valid_access_token().await?;
    println!(
        "Access token acquired ({} chars) — expose() only at the call site.",
        access.expose().len()
    );

    // Persistence is the consumer's job: take a snapshot and store it in your
    // own secure storage (keyring, encrypted file). Restore later with
    // TokenSet::from_stored.
    let stored = session.snapshot();
    println!(
        "Snapshot for secure storage: {} bytes of JSON (redacted Debug: {:?})",
        serde_json::to_vec(&stored)?.len(),
        stored
    );

    // Renewals are observable independently of the request path: persist on
    // every notification so a crash never loses a rotated refresh token.
    let mut renewals = session.subscribe();
    renewals.borrow_and_update();
    println!("Subscription ready — snapshot() on every change is the pull path.");

    Ok(())
}
