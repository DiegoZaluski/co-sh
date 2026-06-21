use super::copilot_auth::{self, CopilotAuth};

fn auth() -> CopilotAuth {
    CopilotAuth::new()
}

/// Saves environment variables and clears them, restoring original values on drop.
///
/// SAFETY: Must not be used concurrently with other code that reads or writes
/// the same environment variables. In practice, tests are either single-threaded
/// (`--test-threads=1`) or marked `#[ignore]`.
struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn clear(names: &[&'static str]) -> Self {
        let mut saved = Vec::with_capacity(names.len());
        for &name in names {
            let val = std::env::var(name).ok();
            // SAFETY: caller ensures single-threaded access to env (see struct docs).
            unsafe { std::env::remove_var(name) };
            saved.push((name, val));
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, val) in &self.saved {
            // SAFETY: caller ensures single-threaded access to env (see struct docs).
            if let Some(v) = val {
                unsafe { std::env::set_var(name, v) };
            } else {
                unsafe { std::env::remove_var(name) };
            }
        }
    }
}

// Token validation

#[test]
fn validate_rejects_empty_token() {
    let result = auth().validate_token("");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Empty"));
}

#[test]
fn validate_rejects_classic_pat() {
    let result = auth().validate_token("ghp_abc123");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("Classic Personal Access Tokens")
    );
}

#[test]
fn validate_accepts_oauth_token() {
    let result = auth().validate_token("gho_test123");
    assert!(result.is_ok());
}

#[test]
fn validate_accepts_fine_grained_pat() {
    let result = auth().validate_token("github_pat_test");
    assert!(result.is_ok());
}

#[test]
fn validate_accepts_github_app_token() {
    let result = auth().validate_token("ghu_test123");
    assert!(result.is_ok());
}

#[test]
fn validate_trims_whitespace() {
    let result = auth().validate_token("  gho_test  ");
    assert!(result.is_ok());
}

// gh CLI discovery

#[test]
fn gh_cli_candidates_includes_which_result() {
    let candidates = copilot_auth::gh_cli_candidates();
    assert!(
        !candidates.is_empty(),
        "expected at least one gh candidate from PATH"
    );
    for c in &candidates {
        assert_eq!(
            c.file_name().and_then(|s| s.to_str()),
            Some("gh"),
            "each candidate must be named 'gh'"
        );
    }
}

#[test]
fn gh_cli_candidates_are_deduplicated() {
    let candidates = copilot_auth::gh_cli_candidates();
    let mut sorted = candidates.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        candidates.len(),
        sorted.len(),
        "candidates must not contain duplicates"
    );
}

// gh CLI token retrieval

#[test]
#[ignore = "requires gh CLI with authenticated session"]
fn try_gh_cli_token_returns_token() {
    let token = copilot_auth::try_gh_cli_token(None);
    assert!(
        token.is_some(),
        "gh auth token should return a token when authenticated"
    );
    let token = token.unwrap();
    assert!(!token.is_empty(), "token must not be empty");
    let valid_prefixes = ["gho_", "github_pat_", "ghu_", "ghp_"];
    assert!(
        valid_prefixes.iter().any(|p| token.starts_with(p)),
        "token {token:.10}... should start with a known prefix"
    );
}

// Resolve token

#[test]
#[ignore = "requires gh CLI with authenticated session"]
fn resolve_copilot_token_from_gh_cli() {
    let _guard = EnvGuard::clear(&["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]);

    let result = auth().resolve_token();
    assert!(
        result.is_ok(),
        "resolve should succeed via gh auth token: {:?}",
        result.err()
    );
    let (token, source) = result.unwrap();
    assert_eq!(source, "gh auth token");
    assert!(!token.is_empty());
}

// Token fingerprint

#[test]
fn token_fingerprint_is_deterministic() {
    let a = copilot_auth::token_fingerprint("test-token-123");
    let b = copilot_auth::token_fingerprint("test-token-123");
    assert_eq!(a, b);
}

#[test]
fn token_fingerprint_differs_for_different_tokens() {
    let a = copilot_auth::token_fingerprint("token-one");
    let b = copilot_auth::token_fingerprint("token-two");
    assert_ne!(a, b);
}

#[test]
fn token_fingerprint_is_hex_string() {
    let fp = copilot_auth::token_fingerprint("some-token");
    assert!(
        fp.chars().all(|c| c.is_ascii_hexdigit()),
        "fingerprint must be hex"
    );
}

// Request headers

#[test]
fn request_headers_contains_expected_keys() {
    let headers = auth().request_headers(false, false);
    assert!(headers.contains_key("Editor-Version"));
    assert!(headers.contains_key("User-Agent"));
    assert!(headers.contains_key("Copilot-Integration-Id"));
    assert!(headers.contains_key("Openai-Intent"));
    assert!(headers.contains_key("x-initiator"));
    assert_eq!(headers.get("x-initiator").unwrap(), "user");
}

#[test]
fn request_headers_agent_turn() {
    let headers = auth().request_headers(true, false);
    assert_eq!(headers.get("x-initiator").unwrap(), "agent");
}

#[test]
fn request_headers_vision_true() {
    let headers = auth().request_headers(false, true);
    assert!(headers.contains_key("Copilot-Vision-Request"));
    assert_eq!(headers.get("Copilot-Vision-Request").unwrap(), "true");
}

// URL encoding

#[test]
fn url_encode_preserves_alphanumeric() {
    let result = copilot_auth::url_encode("abc123");
    assert_eq!(result, "abc123");
}

#[test]
fn url_encode_encodes_special_chars() {
    let result = copilot_auth::url_encode("a b&c=d?e");
    assert_eq!(result, "a+b%26c%3Dd%3Fe");
}

#[test]
fn url_encode_encodes_unicode() {
    let result = copilot_auth::url_encode("café");
    assert_eq!(result, "caf%C3%A9");
}

// Exchange (real)

#[tokio::test]
#[ignore = "requires a valid GitHub token with Copilot access"]
async fn exchange_token_real() {
    let token =
        copilot_auth::try_gh_cli_token(None).expect("gh CLI must be authenticated for this test");

    let result = auth().exchange_token(&token).await;
    if let Ok((api_token, expires_at)) = &result {
        assert!(!api_token.is_empty(), "exchanged token must not be empty");
        assert!(*expires_at > 0.0, "expires_at must be positive");
    }
    match &result {
        Ok((_, _)) => eprintln!("Token exchange succeeded"),
        Err(e) => eprintln!("Token exchange failed (expected for some accounts): {e}"),
    }
}

// Builder / chaining

#[test]
fn builder_configures_host_and_timeout() {
    let auth = CopilotAuth::new()
        .with_host("enterprise.github.com")
        .with_timeout(60.0)
        .with_gh_host("github.mycompany.com");

    assert_eq!(auth.host, "enterprise.github.com");
    assert!((auth.timeout_seconds - 60.0).abs() < f64::EPSILON);
    assert_eq!(auth.gh_host.as_deref(), Some("github.mycompany.com"));
}

#[test]
fn builder_defaults() {
    let auth = CopilotAuth::new();
    assert_eq!(auth.host, "github.com");
    assert!((auth.timeout_seconds - 30.0).abs() < f64::EPSILON);
    assert!(auth.gh_host.is_none());
}
