//! Tests for provider lookup helpers: `has_api_key`, `detect_provider`, the
//! process-lifetime keyring cache, and the local-provider registry.

use super::super::{
    clear_api_key_cache, detect_provider, get_api_key, get_provider, has_api_key,
    invalidate_api_key, is_local_provider, known_local_providers, known_providers,
    normalize_local_base_url,
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

/// Every registered provider has the expected local flag and default port.
#[test]
fn local_providers_registry() {
    let locals: Vec<&str> = known_local_providers().collect();
    let expected = [
        "ollama",
        "llamacpp",
        "lmstudio",
        "vllm",
        "llamafile",
        "koboldcpp",
        "text-generation-webui",
        "localai",
        "jan",
        "gpt4all",
        "aphrodite",
        "sglang",
        "tabbyapi",
    ];
    for name in expected {
        assert!(
            locals.contains(&name),
            "{name} should be registered as local"
        );
        assert!(is_local_provider(name), "{name} should be local");
        let cfg = get_provider(name).expect("provider registered");
        assert!(
            cfg.base_url.contains("localhost"),
            "{name} base_url should be localhost, got {}",
            cfg.base_url
        );
    }
    // A cloud provider is not local.
    assert!(!is_local_provider("openai"));
    // Every local provider is also in the full provider list.
    for name in locals {
        assert!(known_providers().any(|p| p == name), "{name} missing");
    }
}

/// `normalize_local_base_url` appends `/v1` for bare OpenAI-compatible URLs
/// but leaves URLs with a path (and non-OpenAI providers) untouched.
#[test]
fn normalize_local_base_url_adds_v1_when_bare() {
    assert_eq!(
        normalize_local_base_url("llamacpp", "http://127.0.0.1:8080"),
        "http://127.0.0.1:8080/v1"
    );
    assert_eq!(
        normalize_local_base_url("ollama", "http://localhost:11434"),
        "http://localhost:11434/v1"
    );
    assert_eq!(
        normalize_local_base_url("llamacpp", "http://127.0.0.1:8080/v1"),
        "http://127.0.0.1:8080/v1"
    );
    // A trailing slash must not produce a double slash before `/v1`.
    assert_eq!(
        normalize_local_base_url("llamacpp", "http://127.0.0.1:8080/"),
        "http://127.0.0.1:8080/v1"
    );
    assert_eq!(
        normalize_local_base_url("ollama", "http://localhost:11434/"),
        "http://localhost:11434/v1"
    );
    // A custom path is kept verbatim.
    assert_eq!(
        normalize_local_base_url("llamacpp", "http://127.0.0.1:8080/openai"),
        "http://127.0.0.1:8080/openai"
    );
    // Unknown provider: returned verbatim.
    assert_eq!(
        normalize_local_base_url("nope", "http://127.0.0.1:1"),
        "http://127.0.0.1:1"
    );
}
