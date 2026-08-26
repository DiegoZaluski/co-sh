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

/// Provider name of the OpenCode Zen gateway (`https://opencode.ai/zen/v1`).
///
/// The gateway deliberately serves an anonymous free tier: requests carrying
/// [`ZEN_PUBLIC_KEY`] are answered only for models flagged `allowAnonymous`
/// server-side and are rate-limited per IP (billing source `"anonymous"` in
/// the gateway's own code). The official OpenCode client sends the same
/// sentinel whenever no account key is configured.
///
/// Compliance rules baked into cosh's use of it:
/// - a real account key ALWAYS wins over the sentinel;
/// - the anonymous tier is used only after an explicit one-time opt-in;
/// - we identify ourselves with our own User-Agent and never imitate the
///   official client's headers to obtain its rate-limit bucket.
pub const ZEN_PROVIDER: &str = "opencode";

/// Sentinel bearer token accepted by the Zen gateway for ANONYMOUS access:
/// only zero-cost models are served and per-IP limits apply. Never sent when
/// an account key resolved from keyring/environment.
pub const ZEN_PUBLIC_KEY: &str = "public";

/// Model IDs served on the Zen anonymous (free) tier at the time of writing,
/// enforced client-side so a keyless session can never request a billed
/// model (the gateway independently rejects those anyway). Callers should
/// intersect this list with the live `/models` catalog: models rotate as
/// their free periods end.
///
/// Deliberately excludes `muse-spark-1.2-contributor-free`, whose data
/// policy grants training rights on prompts/completions.
pub const ZEN_FREE_MODELS: &[&str] = &[
    "big-pickle",
    "x-preview-f-free",
    "mimo-v2.5-free",
    "hy3-free",
    "nemotron-3-ultra-free",
    "nemotron-3.5-lightning-free",
];

/// Honest `User-Agent` for OpenCode Zen requests: identifies co-sh as the
/// calling client. Anonymous traffic earns the gateway's stricter public
/// rate-limit bucket; keeping that bucket instead of imitating another
/// client's identity is a hard compliance requirement.
pub const ZEN_USER_AGENT: &str = concat!("cosh/", env!("CARGO_PKG_VERSION"));

/// Whether `model` is served on the Zen anonymous (free) tier.
#[must_use]
pub fn is_zen_free_model(model: &str) -> bool {
    ZEN_FREE_MODELS.contains(&model)
}

/// Process-wide opt-in state for the Zen anonymous free tier.
static ZEN_PUBLIC_TIER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enable/disable the Zen anonymous free tier for EVERY connector in this
/// process. The UI sets this once from its persisted opt-in so connectors
/// built deep inside consumers (harness fallback chains, compaction,
/// session titles) inherit the user's answer without each construction site
/// needing to thread it through. Per-connector
/// [`Connector::with_zen_public_tier`](crate::connector::Connector::with_zen_public_tier)
/// overrides remain independent for tests and special cases.
pub fn set_zen_public_tier_enabled(enabled: bool) {
    ZEN_PUBLIC_TIER.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the process-wide Zen free-tier opt-in is currently enabled.
#[must_use]
pub fn zen_public_tier_enabled() -> bool {
    ZEN_PUBLIC_TIER.load(std::sync::atomic::Ordering::Relaxed)
}

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
    /// Whether the endpoint runs on the user's machine (`localhost`). Local
    /// providers have no API key: they are configured with a base URL instead.
    pub local: bool,
}

impl ProviderConfig {
    const fn cloud(
        name: &'static str,
        family: Family,
        base_url: &'static str,
        default_model: &'static str,
    ) -> Self {
        Self {
            name,
            family,
            base_url,
            default_model,
            needs_extra_headers: false,
            local: false,
        }
    }

    /// Like [`cloud`](Self::cloud) but allows setting `needs_extra_headers`
    /// (currently only OpenRouter requires this).
    const fn cloud_extra(
        name: &'static str,
        family: Family,
        base_url: &'static str,
        default_model: &'static str,
        needs_extra_headers: bool,
    ) -> Self {
        Self {
            name,
            family,
            base_url,
            default_model,
            needs_extra_headers,
            local: false,
        }
    }

    const fn local(name: &'static str, base_url: &'static str) -> Self {
        Self {
            name,
            family: Family::OpenAICompatible,
            base_url,
            default_model: "llama3.3",
            needs_extra_headers: false,
            local: true,
        }
    }
}

const PROVIDERS: &[(&str, ProviderConfig)] = &[
    // ── Cloud providers ──────────────────────────────────────────────
    (
        "openai",
        ProviderConfig::cloud(
            "openai",
            Family::OpenAICompatible,
            "https://api.openai.com/v1",
            "gpt-4o-mini",
        ),
    ),
    (
        "groq",
        ProviderConfig::cloud(
            "groq",
            Family::OpenAICompatible,
            "https://api.groq.com/openai/v1",
            "llama-3.3-70b-versatile",
        ),
    ),
    (
        "mistral",
        ProviderConfig::cloud(
            "mistral",
            Family::OpenAICompatible,
            "https://api.mistral.ai/v1",
            "mistral-small-latest",
        ),
    ),
    (
        "together",
        ProviderConfig::cloud(
            "together",
            Family::OpenAICompatible,
            "https://api.together.xyz/v1",
            "meta-llama/Llama-3.1-8B-Instruct-Turbo",
        ),
    ),
    (
        "openrouter",
        ProviderConfig::cloud_extra(
            "openrouter",
            Family::OpenAICompatible,
            "https://openrouter.ai/api/v1",
            "qwen/qwen-2.5-72b-instruct",
            true,
        ),
    ),
    (
        "xai",
        ProviderConfig::cloud(
            "xai",
            Family::OpenAICompatible,
            "https://api.x.ai/v1",
            "grok-2-1212",
        ),
    ),
    (
        "deepseek",
        ProviderConfig::cloud(
            "deepseek",
            Family::OpenAICompatible,
            "https://api.deepseek.com/v1",
            "deepseek-chat",
        ),
    ),
    (
        "perplexity",
        ProviderConfig::cloud(
            "perplexity",
            Family::OpenAICompatible,
            "https://api.perplexity.ai",
            "llama-3.1-sonar-small-128k-chat",
        ),
    ),
    (
        "fireworks",
        ProviderConfig::cloud(
            "fireworks",
            Family::OpenAICompatible,
            "https://api.fireworks.ai/inference/v1",
            "qwen2.5-72b-instruct",
        ),
    ),
    (
        "cohere",
        ProviderConfig::cloud(
            "cohere",
            Family::OpenAICompatible,
            "https://api.cohere.com/compatibility/v1",
            "command-r-plus",
        ),
    ),
    (
        "huggingface",
        ProviderConfig::cloud(
            "huggingface",
            Family::OpenAICompatible,
            "https://router.huggingface.co/v1",
            "meta-llama/Llama-3.3-70B-Instruct",
        ),
    ),
    (
        "sambanova",
        ProviderConfig::cloud(
            "sambanova",
            Family::OpenAICompatible,
            "https://api.sambanova.ai/v1",
            "Meta-Llama-3.1-8B-Instruct",
        ),
    ),
    (
        "poe",
        ProviderConfig::cloud(
            "poe",
            Family::OpenAICompatible,
            "https://api.poe.com/v1",
            "GPT-4o",
        ),
    ),
    (
        "cerebras",
        ProviderConfig::cloud(
            "cerebras",
            Family::OpenAICompatible,
            "https://api.cerebras.ai/v1",
            "llama-3.1-8b-chat-completion",
        ),
    ),
    (
        "nvidia",
        ProviderConfig::cloud(
            "nvidia",
            Family::OpenAICompatible,
            "https://integrate.api.nvidia.com/v1",
            "meta/llama-3.1-8b-instruct",
        ),
    ),
    (
        "anyscale",
        ProviderConfig::cloud(
            "anyscale",
            Family::OpenAICompatible,
            "https://api.endpoints.anyscale.com/v1",
            "meta-llama/Llama-3.1-8B-Instruct",
        ),
    ),
    (
        "vercel",
        ProviderConfig::cloud(
            "vercel",
            Family::OpenAICompatible,
            "https://ai-gateway.vercel.sh/v1",
            "meta-llama/Llama-3.1-8B-Instruct",
        ),
    ),
    (
        "cloudflare",
        ProviderConfig::cloud(
            "cloudflare",
            Family::OpenAICompatible,
            "https://gateway.ai.cloudflare.com/v1/_/cloudflare/workers-ai/openai",
            "@cf/meta/llama-3.1-8b-instruct",
        ),
    ),
    (
        "azure",
        ProviderConfig::cloud("azure", Family::OpenAICompatible, "", "gpt-4o"),
    ),
    // Non-OpenAI cloud providers
    // gemini-3.6-flash: stable v1 default — older models (1.5-flash, 2.5-flash)
    // return 404 on v1 and 2.0-flash is rate-limited on free tiers.
    (
        "gemini",
        ProviderConfig::cloud(
            "gemini",
            Family::Gemini,
            "https://generativelanguage.googleapis.com/v1",
            "gemini-3.6-flash",
        ),
    ),
    (
        "claude",
        ProviderConfig::cloud(
            "claude",
            Family::Claude,
            "https://api.anthropic.com/v1",
            "claude-sonnet-4-6",
        ),
    ),
    (
        "zai",
        ProviderConfig::cloud(
            "zai",
            Family::OpenAICompatible,
            "https://api.z.ai/api/paas/v4",
            "",
        ),
    ),
    (
        "charm",
        ProviderConfig::cloud(
            "charm",
            Family::OpenAICompatible,
            "https://hyper.charm.land/v1",
            "",
        ),
    ),
    (
        "opencode",
        ProviderConfig::cloud(
            "opencode",
            Family::OpenAICompatible,
            "https://opencode.ai/zen/v1",
            "x-preview-f-free",
        ),
    ),
    // ── Local providers (configured by URL, no API key) ──────────────
    (
        "ollama",
        ProviderConfig::local("ollama", "http://localhost:11434/v1"),
    ),
    (
        "lmstudio",
        ProviderConfig::local("lmstudio", "http://localhost:1234/v1"),
    ),
    (
        "vllm",
        ProviderConfig::local("vllm", "http://localhost:8000/v1"),
    ),
    (
        "llamacpp",
        ProviderConfig::local("llamacpp", "http://localhost:8080/v1"),
    ),
    (
        "llamafile",
        ProviderConfig::local("llamafile", "http://localhost:8080/v1"),
    ),
    (
        "koboldcpp",
        ProviderConfig::local("koboldcpp", "http://localhost:5001/v1"),
    ),
    (
        "text-generation-webui",
        ProviderConfig::local("text-generation-webui", "http://localhost:5000/v1"),
    ),
    (
        "localai",
        ProviderConfig::local("localai", "http://localhost:8080/v1"),
    ),
    (
        "jan",
        ProviderConfig::local("jan", "http://localhost:1337/v1"),
    ),
    (
        "gpt4all",
        ProviderConfig::local("gpt4all", "http://localhost:4891/v1"),
    ),
    (
        "aphrodite",
        ProviderConfig::local("aphrodite", "http://localhost:2242/v1"),
    ),
    (
        "sglang",
        ProviderConfig::local("sglang", "http://localhost:30000/v1"),
    ),
    (
        "tabbyapi",
        ProviderConfig::local("tabbyapi", "http://localhost:5000/v1"),
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
    ("opencode", "OPENCODE_API_KEY"),
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

/// Whether a provider runs a local model server (`localhost`) with no API
/// key. Local providers are configured with a base URL instead of a key.
#[must_use]
pub fn is_local_provider(name: &str) -> bool {
    get_provider(name).is_some_and(|cfg| cfg.local)
}

/// Return all known local provider names (ollama, llamacpp, lmstudio, …).
///
/// These are configured with a base URL (host + port) rather than an API key.
pub fn known_local_providers() -> impl Iterator<Item = &'static str> {
    PROVIDERS
        .iter()
        .filter(|(_, cfg)| cfg.local)
        .map(|(name, _)| *name)
}

/// Normalize a user-supplied local server URL for an OpenAI-compatible
/// provider: if the URL has no path, append `/v1` so it matches the endpoint
/// the server exposes (`/v1/chat/completions`). Non-OpenAI-compatible
/// providers and URLs that already carry a path are returned verbatim.
#[must_use]
pub fn normalize_local_base_url(provider: &str, url: &str) -> String {
    // Strip a trailing slash so bare URLs with `/` don't produce `//v1`.
    let url = url.trim().trim_end_matches('/');
    let Some(cfg) = get_provider(provider) else {
        return url.to_string();
    };
    if cfg.family != Family::OpenAICompatible {
        return url.to_string();
    }
    // Use a proper URL parser instead of fragile string splitting — handles
    // ports, query strings, and edge cases like `http://` or `https://host/`.
    match reqwest::Url::parse(url) {
        Ok(parsed) if parsed.path() == "/" || parsed.path().is_empty() => {
            format!("{url}/v1")
        }
        _ => url.to_string(),
    }
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
