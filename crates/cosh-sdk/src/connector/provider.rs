//! Provider registry — known LLM backends, their base URLs, default models,
//! and API key environment variable names.
//!
use keyring::Entry;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use zeroize::Zeroizing;

/// Default keyring service under which cosh stores its API keys.
///
/// `Connector`'s parameters already default `service_keyring` to this value,
/// so ordinary consumers don't need to reference it — it exists so the SDK
/// reads keys under the same service string that the TUI writes via
/// `save_provider_api_key`. Override only builders that pass a custom
/// service: [`Connector::with_service_keyring`](crate::connector::Connector::with_service_keyring).
pub const COSH_SERVICE: &str = "cosh";

/// Process-lifetime cache mapping `(service, env_var)` to the resolved key.
type KeyringCache = HashMap<(String, String), Zeroizing<String>>;

/// Process-lifetime cache of resolved keyring keys, keyed by
/// `(service, env_var)`.
///
/// The first successful keyring lookup for a given key stays alive in process
/// memory, so repeated resolutions (model listing, chat, provider detection)
/// avoid re-hitting the OS credential store on every call. Only successful
/// lookups are cached — misses still consult the keyring each time, keeping
/// keys added/updated outside the app visible immediately.
///
/// Cache omitting Zeroizing would hold keys in plain `String` for the whole
/// process too; `Zeroizing` makes sure the memory is wiped when an entry is
/// invalidated or dropped instead.
///
/// Use [`invalidate_api_key`] after storing or updating a key so the cache
/// never serves a stale value from a previous save.
static KEYRING_CACHE: LazyLock<Mutex<KeyringCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read a key from the OS keyring, caching successful lookups so repeat
/// resolutions don't re-hit the credential store.
fn keyring_lookup(service: &str, env_var: &str) -> Option<String> {
    let key = (service.to_owned(), env_var.to_owned());
    if let Some(cached) = KEYRING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return Some((**cached).clone());
    }

    let found = Entry::new(service, env_var)
        .and_then(|e| e.get_password())
        .ok()?;
    KEYRING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, Zeroizing::new(found.clone()));
    Some(found)
}

/// Forget cached keys for an env var, e.g. right after saving/updating that
/// provider's key so the freshest value is resolved next time.
pub fn invalidate_api_key(env_var: &str) {
    KEYRING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(_, var), _| var != env_var);
}

/// Forget every cached key. The process keeps resolving new lookups from the
/// OS keyring afterwards.
pub fn clear_api_key_cache() {
    KEYRING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    OpenAICompatible,
    Gemini,
    Claude,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderConfig {
    pub name: &'static str,
    pub family: Family,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub needs_extra_headers: bool,
}

const PROVIDERS: &[(&str, ProviderConfig)] = &[
    (
        "openai",
        ProviderConfig {
            name: "openai",
            family: Family::OpenAICompatible,
            base_url: "https://api.openai.com/v1",
            default_model: "gpt-4o-mini",
            needs_extra_headers: false,
        },
    ),
    (
        "groq",
        ProviderConfig {
            name: "groq",
            family: Family::OpenAICompatible,
            base_url: "https://api.groq.com/openai/v1",
            default_model: "llama-3.3-70b-versatile",
            needs_extra_headers: false,
        },
    ),
    (
        "mistral",
        ProviderConfig {
            name: "mistral",
            family: Family::OpenAICompatible,
            base_url: "https://api.mistral.ai/v1",
            default_model: "mistral-small-latest",
            needs_extra_headers: false,
        },
    ),
    (
        "together",
        ProviderConfig {
            name: "together",
            family: Family::OpenAICompatible,
            base_url: "https://api.together.xyz/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct-Turbo",
            needs_extra_headers: false,
        },
    ),
    (
        "openrouter",
        ProviderConfig {
            name: "openrouter",
            family: Family::OpenAICompatible,
            base_url: "https://openrouter.ai/api/v1",
            default_model: "qwen/qwen-2.5-72b-instruct",
            needs_extra_headers: true,
        },
    ),
    (
        "xai",
        ProviderConfig {
            name: "xai",
            family: Family::OpenAICompatible,
            base_url: "https://api.x.ai/v1",
            default_model: "grok-2-1212",
            needs_extra_headers: false,
        },
    ),
    (
        "deepseek",
        ProviderConfig {
            name: "deepseek",
            family: Family::OpenAICompatible,
            base_url: "https://api.deepseek.com/v1",
            default_model: "deepseek-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "perplexity",
        ProviderConfig {
            name: "perplexity",
            family: Family::OpenAICompatible,
            base_url: "https://api.perplexity.ai",
            default_model: "llama-3.1-sonar-small-128k-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "fireworks",
        ProviderConfig {
            name: "fireworks",
            family: Family::OpenAICompatible,
            base_url: "https://api.fireworks.ai/inference/v1",
            default_model: "qwen2.5-72b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cohere",
        ProviderConfig {
            name: "cohere",
            family: Family::OpenAICompatible,
            base_url: "https://api.cohere.com/compatibility/v1",
            default_model: "command-r-plus",
            needs_extra_headers: false,
        },
    ),
    (
        "huggingface",
        ProviderConfig {
            name: "huggingface",
            family: Family::OpenAICompatible,
            base_url: "https://router.huggingface.co/v1",
            default_model: "meta-llama/Llama-3.3-70B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "sambanova",
        ProviderConfig {
            name: "sambanova",
            family: Family::OpenAICompatible,
            base_url: "https://api.sambanova.ai/v1",
            default_model: "Meta-Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "poe",
        ProviderConfig {
            name: "poe",
            family: Family::OpenAICompatible,
            base_url: "https://api.poe.com/v1",
            default_model: "GPT-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "cerebras",
        ProviderConfig {
            name: "cerebras",
            family: Family::OpenAICompatible,
            base_url: "https://api.cerebras.ai/v1",
            default_model: "llama-3.1-8b-chat-completion",
            needs_extra_headers: false,
        },
    ),
    (
        "nvidia",
        ProviderConfig {
            name: "nvidia",
            family: Family::OpenAICompatible,
            base_url: "https://integrate.api.nvidia.com/v1",
            default_model: "meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "anyscale",
        ProviderConfig {
            name: "anyscale",
            family: Family::OpenAICompatible,
            base_url: "https://api.endpoints.anyscale.com/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "vercel",
        ProviderConfig {
            name: "vercel",
            family: Family::OpenAICompatible,
            base_url: "https://ai-gateway.vercel.sh/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cloudflare",
        ProviderConfig {
            name: "cloudflare",
            family: Family::OpenAICompatible,
            base_url: "https://gateway.ai.cloudflare.com/v1/_/cloudflare/workers-ai/openai",
            default_model: "@cf/meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "azure",
        ProviderConfig {
            name: "azure",
            family: Family::OpenAICompatible,
            base_url: "",
            default_model: "gpt-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "ollama",
        ProviderConfig {
            name: "ollama",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:11434/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "lmstudio",
        ProviderConfig {
            name: "lmstudio",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:1234/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "vllm",
        ProviderConfig {
            name: "vllm",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:8000/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "llamacpp",
        ProviderConfig {
            name: "llamacpp",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:8080/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "gemini",
        ProviderConfig {
            name: "gemini",
            family: Family::Gemini,
            // Stable v1 API surface. The gemini module keeps an internal
            // v1beta switch for testing experimental features — library
            // users cannot select the version via the public API.
            base_url: "https://generativelanguage.googleapis.com/v1",
            // Current default on the v1 API. Older defaults are gone from
            // v1: gemini-1.5-flash/2.5-flash return 404 ("not found for
            // API version v1") and gemini-2.0-flash is rate-limited on
            // free tiers — the docs-recommended gemini-3.6-flash works.
            default_model: "gemini-3.6-flash",
            needs_extra_headers: false,
        },
    ),
    (
        "claude",
        ProviderConfig {
            name: "claude",
            family: Family::Claude,
            base_url: "https://api.anthropic.com/v1",
            default_model: "claude-sonnet-4-6",
            needs_extra_headers: false,
        },
    ),
    (
        "zai",
        ProviderConfig {
            name: "zai",
            family: Family::OpenAICompatible,
            base_url: "https://api.z.ai/api/paas/v4",
            default_model: "",
            needs_extra_headers: false,
        },
    ),
    (
        "charm",
        ProviderConfig {
            name: "charm",
            family: Family::OpenAICompatible,
            base_url: "https://hyper.charm.land/v1",
            default_model: "",
            needs_extra_headers: false,
        },
    ),
];

const API_KEY_ENVS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("huggingface", "HUGGINGFACE_API_KEY"),
    ("nvidia", "NVIDIA_API_KEY"),
    ("github", "GITHUB_TOKEN"),
    ("cloudflare", "CLOUDFLARE_API_KEY"),
    ("fireworks", "FIREWORKS_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("perplexity", "PERPLEXITY_API_KEY"),
    ("cohere", "COHERE_API_KEY"),
    ("sambanova", "SAMBANOVA_API_KEY"),
    ("poe", "POE_API_KEY"),
    ("anyscale", "ANYSCALE_API_KEY"),
    ("vercel", "VERCEL_API_KEY"),
    ("azure", "AZURE_OPENAI_KEY"),
    ("gemini", "GEMINI_API_KEY"),
    ("claude", "ANTHROPIC_API_KEY"),
    ("zai", "ZAI_API_KEY"),
    ("charm", "CHARM_API_KEY"),
];

pub fn get_provider(name: &str) -> Option<&'static ProviderConfig> {
    PROVIDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, config)| config)
}

pub fn get_api_key(provider: &str, service: Option<&str>) -> Option<String> {
    let env_var = API_KEY_ENVS
        .iter()
        .find(|(key, _)| *key == provider)
        .map_or("OPENAI_API_KEY", |(_, var)| *var);

    // The OS keyring is the authoritative store for keys saved explicitly
    // through the app (ADD Provider). Prefer it over the process environment
    // so stale shell/.env exports don't shadow the key the user configured.
    // Environment variables remain a fallback for providers never stored.
    let service = service.unwrap_or(COSH_SERVICE);
    if let Some(key) = keyring_lookup(service, env_var) {
        return Some(key);
    }

    std::env::var(env_var).ok()
}

/// Return all known provider names.
///
/// Useful for enumerating providers in the TUI front-end or for batch
/// operations (e.g. fetching model lists from every provider).
pub fn known_providers() -> impl Iterator<Item = &'static str> {
    PROVIDERS.iter().map(|(name, _)| *name)
}

/// Return the environment variable name for a given provider's API key.
///
/// Returns `None` if the provider is unknown.
#[must_use]
pub fn get_provider_env_var(provider: &str) -> Option<&'static str> {
    API_KEY_ENVS
        .iter()
        .find(|(key, _)| *key == provider)
        .map(|(_, var)| *var)
}

/// Return all known providers with their API key environment variable names.
///
/// Useful for UI rendering that needs both the provider name and its key env var.
pub fn known_providers_with_env() -> impl Iterator<Item = (&'static str, &'static str)> {
    API_KEY_ENVS.iter().map(|(name, env)| (*name, *env))
}

/// Whether a provider has an API key available, either from the OS credential
/// store (keyring) under the cosh service or from its environment variable.
///
/// Useful for the UI to decide whether a provider is "configured" without
/// having to distinguish between the two storage backends. Resolution order
/// matches [`get_api_key`]: keyring first, then environment.
#[must_use]
pub fn has_api_key(provider: &str) -> bool {
    get_api_key(provider, Some(COSH_SERVICE)).is_some()
}

/// Return the first provider with an available API key.
///
/// Checks both the environment variables and the OS credential store. The
/// iteration order follows the registration order in the provider table.
#[must_use]
pub fn detect_provider() -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(name, _)| has_api_key(name))
        .map(|(name, _)| *name)
}
