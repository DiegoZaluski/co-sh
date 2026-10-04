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

/// REGRESSION (empty env export): a shell profile exporting `FOO_API_KEY=`
/// (placeholder, template leak, cleanup leftover) must NOT count as a
/// credential — the empty string used to ride `resolve_env_var_key` as
/// `Some("")`, making `has_api_key` report the provider as configured. The
/// keyless gates (pre-send gateway offer, error-driven offer) then stayed
/// silent while the send failed deep inside the HTTP call with a
/// non-auth-shaped error. An empty value resolves to None everywhere.
///
/// Hermeticity: `has_api_key` consults the REAL `cosh` service keyring
/// before the env var, so a machine with a key saved through the app's ADD
/// Provider flow would spuriously flip these assertions — and DELETING the
/// entry would destroy a real user credential. The prologue SKIPS the test
/// when a non-blank keyring key exists (mirroring the cache tests'
/// no-keyring skip); blank entries need no cleanup — the filter under test
/// ignores them by construction.
#[tokio::test]
async fn empty_env_value_is_not_a_credential() {
    let _lock = ENV_LOCK.lock().await;
    for env_var in ["OPENAI_API_KEY", "OPENCODE_API_KEY"] {
        if let Ok(entry) = keyring::Entry::new(super::super::COSH_SERVICE, env_var)
            && let Ok(stored) = entry.get_password()
            && !stored.trim().is_empty()
        {
            eprintln!("skipping: a real {env_var} key exists in the cosh keyring");
            return;
        }
    }
    let _guard = EnvGuard::set("OPENAI_API_KEY", "");
    assert!(
        !has_api_key("openai"),
        "an empty env export is a placeholder, not a configured key"
    );
    assert_eq!(
        get_api_key("openai", None),
        None,
        "get_api_key must not hand out the empty string either"
    );
    // The fallback-var path filters symmetrically.
    let _guard_go = EnvGuard::set("OPENCODE_API_KEY", "");
    assert!(
        !has_api_key("opencode-go"),
        "the fallback env var is filtered the same way"
    );
    // Whitespace-only is the same placeholder class (a quoted-space export
    // or a stray space pasted into the ADD Provider input).
    let _guard_ws = EnvGuard::set("OPENAI_API_KEY", "   ");
    assert!(
        !has_api_key("openai"),
        "a blank env export is not a credential either"
    );
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
/// read. A new key written through cosh becomes visible after invalidation.
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

/// Repeated absent-key checks must not hit the OS store on every UI frame.
/// An explicit save invalidates the cached miss immediately.
#[tokio::test]
async fn keyring_miss_is_cached_until_invalidated() {
    let _lock = ENV_LOCK.lock().await;
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let service = format!("cosh-tests-miss-{}", std::process::id());
    let entry = match keyring::Entry::new(&service, "OPENAI_API_KEY") {
        Ok(entry) => entry,
        Err(_) => return,
    };
    if entry.set_password("probe").is_err() {
        return;
    }
    if entry.delete_credential().is_err() {
        return;
    }

    clear_api_key_cache();
    assert_eq!(get_api_key("openai", Some(&service)), None);
    entry.set_password("new-key").unwrap();
    assert_eq!(get_api_key("openai", Some(&service)), None);

    invalidate_api_key("OPENAI_API_KEY");
    assert_eq!(
        get_api_key("openai", Some(&service)).as_deref(),
        Some("new-key")
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
