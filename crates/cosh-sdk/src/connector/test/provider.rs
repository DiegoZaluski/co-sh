//! Tests for provider lookup helpers: `has_api_key`, `detect_provider`, and
//! the process-lifetime keyring cache.

use super::super::{
    clear_api_key_cache, detect_provider, get_api_key, has_api_key, invalidate_api_key,
};
use super::common::{ENV_LOCK, EnvGuard};

/// `has_api_key` reports `true` when the provider's env var is set.
#[tokio::test]
async fn has_api_key_env() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::set("OPENAI_API_KEY", "sk-test");
    assert!(has_api_key("openai"));
}

/// `detect_provider` picks the first provider (registration order) with a key.
#[tokio::test]
async fn detect_provider_env() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::set("OPENAI_API_KEY", "sk-test");
    assert_eq!(detect_provider(), Some("openai"));
}

/// Successful keyring lookups are cached for the process lifetime and served
/// without re-hitting the OS store; invalidating the env var forces a fresh
/// read. Misses are not cached, so a later write is visible immediately.
#[tokio::test]
async fn get_api_key_caches_hits_until_invalidated() {
    let _lock = ENV_LOCK.lock().await;
    // Keep the env var out so only the keyring path is exercised.
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let service = format!("cosh-tests-cache-{}", std::process::id());
    let entry = match keyring::Entry::new(&service, "OPENAI_API_KEY") {
        Ok(entry) => entry,
        Err(err) => {
            eprintln!("skipping: no keyring store available: {err}");
            return;
        }
    };
    if entry.set_password("first").is_err() {
        eprintln!("skipping: no keyring store available");
        return;
    }

    assert_eq!(
        get_api_key("openai", Some(&service)).as_deref(),
        Some("first")
    );
    // Second resolution comes from the in-process cache.
    assert_eq!(
        get_api_key("openai", Some(&service)).as_deref(),
        Some("first")
    );

    invalidate_api_key("OPENAI_API_KEY");
    let _ = entry.set_password("second");
    assert_eq!(
        get_api_key("openai", Some(&service)).as_deref(),
        Some("second")
    );

    let _ = entry.delete_credential();
    clear_api_key_cache();
}
