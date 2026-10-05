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

/// Provider name of the DOCUMENTED OpenCode Zen gateway
/// (`https://opencode.ai/zen/v1`, `https://opencode.ai/docs/zen`).
///
/// Key-based access to the full Zen catalog: an account key from
/// `opencode.ai/auth` unlocks every model. A missing key fails with
/// [`ConnectorError::MissingApiKey`].
pub const OPENCODE_ZEN_PROVIDER: &str = "zen";

/// Provider name of the OpenCode Go gateway
/// (`https://opencode.ai/zen/go/v1`, `https://opencode.ai/docs/go`).
///
/// A $10/month subscription serving popular open coding models (GLM, Kimi,
/// Qwen, DeepSeek, MiniMax, MiMo, …) behind the same OpenAI-compatible
/// surface as Zen. Subscriptions are managed through an OpenCode Zen
/// account, so the key copied from `opencode.ai/auth` works on both
/// gateways — `OPENCODE_GO_API_KEY` is tried first and
/// `OPENCODE_API_KEY` is accepted as a fallback. There is no anonymous
/// tier: a key is always required.
pub const OPENCODE_GO_PROVIDER: &str = "opencode-go";

/// Honest `User-Agent` for OpenCode gateway requests: identifies co-sh as
/// the calling client instead of imitating another client's identity.
pub const ZEN_USER_AGENT: &str = concat!("cosh/", env!("CARGO_PKG_VERSION"));

/// Whether the provider is hosted on the OpenCode gateway infrastructure
/// (`opencode.ai/zen/...`): the documented Zen gateway
/// ([`OPENCODE_ZEN_PROVIDER`]) and the Go subscription gateway
/// ([`OPENCODE_GO_PROVIDER`]). Both identify themselves with the
/// honest [`ZEN_USER_AGENT`].
#[must_use]
pub fn is_opencode_gateway(name: &str) -> bool {
    matches!(name, OPENCODE_ZEN_PROVIDER | OPENCODE_GO_PROVIDER)
}

/// A missing credential is cached briefly so a UI render does not query the
/// OS keyring on every frame. Explicit saves/removals invalidate the entry.
const KEYRING_MISS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

enum CachedKeyringValue {
    Found(Zeroizing<String>),
    Missing(std::time::Instant),
}

/// Process-lifetime cache mapping `(service, env_var)` to a key or recent miss.
type KeyringCache = HashMap<(String, String), CachedKeyringValue>;

/// Process-lifetime cache of resolved keyring keys, keyed by
/// `(service, env_var)`.
///
/// The first successful keyring lookup for a given key stays alive in process
/// memory, so repeated resolutions (model listing, chat, provider detection)
/// avoid re-hitting the OS credential store on every call. Successful
/// lookups are cached for the process lifetime. Misses expire after 30 seconds;
/// keys saved through cosh invalidate their entry immediately.
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
    {
        let cache = KEYRING_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match cache.get(&key) {
            Some(CachedKeyringValue::Found(value)) => return Some((**value).clone()),
            Some(CachedKeyringValue::Missing(at)) if at.elapsed() < KEYRING_MISS_TTL => {
                return None;
            }
            _ => {}
        }
    }

    let found = Entry::new(service, env_var)
        .and_then(|e| e.get_password())
        .ok();
    KEYRING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            key,
            match &found {
                Some(value) => CachedKeyringValue::Found(Zeroizing::new(value.clone())),
                None => CachedKeyringValue::Missing(std::time::Instant::now()),
            },
        );
    found
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
    /// The OpenAI-native Responses API (`/v1/responses`). Only the `openai`
    /// provider uses this family; it is what exposes reasoning for OpenAI's
    /// reasoning models. `embed` and `list_models` still route through the
    /// shared `/v1/embeddings` and `/v1/models` endpoints.
    OpenAi,
    Gemini,
    Claude,
    /// GitHub's official Copilot agent runtime, driven through the official
    /// `github-copilot-sdk` crate (JSON-RPC over a spawned runtime process
    /// — no HTTP chat endpoint). An ISOLATED family: it shares none of the
    /// OpenAI-compatible request machinery, only the connector's output
    /// types. `embed` is unsupported; `list_models` does NOT use the
    /// runtime — it is a direct HTTP chain (GitHub token exchange, then
    /// the shared chat catalog), because the runtime's `models.list` RPC
    /// reads the agent-mode catalog, which stays empty for Free-tier
    /// accounts even though the account can chat fine (see
    /// `copilot::models`).
    Copilot,
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
            Family::OpenAi,
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
    // Amazon Bedrock through its official OpenAI-compatible endpoint. The
    // registry URL only pins the DEFAULT region — the live base URL is
    // resolved per request by [`bedrock_base_url`] from the standard AWS
    // environment variables (`AWS_REGION` / `AWS_DEFAULT_REGION`), and an
    // explicit `params.base_url` always wins. Auth rides the shared
    // `Authorization: Bearer` path with the Bedrock API key
    // (`AWS_BEARER_TOKEN_BEDROCK`) — the SigV4/IAM setup needs the
    // token-bearing env export or an explicit base URL.
    (
        "bedrock",
        ProviderConfig::cloud(
            "bedrock",
            Family::OpenAICompatible,
            "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1",
            // Documented Chat Completions model on bedrock-runtime (the
            // endpoint's own docs use it as the example).
            "openai.gpt-oss-120b-1:0",
        ),
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
        "copilot",
        ProviderConfig::cloud(
            "copilot",
            Family::Copilot,
            // No HTTP endpoint: the family drives the official Copilot
            // runtime over JSON-RPC (see connector/copilot). The URL field
            // stays a stable identifier pointing at the runtime's home.
            "https://github.com/copilot",
            // Runtime default; overridable via `params.model`.
            "claude-sonnet-4.5",
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
    // Documented OpenCode Zen gateway: strictly key-based — the full
    // catalog is served to the account key from `opencode.ai/auth`.
    (
        "zen",
        ProviderConfig::cloud(
            "zen",
            Family::OpenAICompatible,
            "https://opencode.ai/zen/v1",
            "gpt-5.6-luna",
        ),
    ),
    // OpenCode Go: $10/month subscription for open coding models, served
    // from the same account as Zen (hence the shared-key fallback).
    (
        "opencode-go",
        ProviderConfig::cloud(
            "opencode-go",
            Family::OpenAICompatible,
            "https://opencode.ai/zen/go/v1",
            "glm-5.3-flash",
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
    ("copilot", "GITHUB_TOKEN"),
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
    ("zen", "OPENCODE_API_KEY"),
    ("opencode-go", "OPENCODE_GO_API_KEY"),
    // AWS's Bedrock bearer token: the API-key auth mode of the OpenAI-
    // compatible endpoint. IAM/SigV4 setups must export this token (or point
    // the connector at a proxy via `with_base_url`) — no SigV4 signing here.
    ("bedrock", "AWS_BEARER_TOKEN_BEDROCK"),
];

/// Secondary key sources for providers whose subscriptions share an account:
/// a Go subscription is managed through the same OpenCode Zen account, so a
/// key saved under the Zen variable also unlocks the Go gateway. Consulted
/// only after the provider's own env var (keyring then environment) misses.
const API_KEY_FALLBACK_ENVS: &[(&str, &str)] = &[("opencode-go", "OPENCODE_API_KEY")];

pub fn get_provider(name: &str) -> Option<&'static ProviderConfig> {
    PROVIDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, config)| config)
}

/// Keyring first, environment second — the shared resolution used for both
/// a provider's primary env var and its fallback (see
/// [`API_KEY_FALLBACK_ENVS`]). An empty (or blank) value is NOT a
/// credential: a shell profile exporting `FOO_API_KEY=` (placeholder,
/// template leak, cleanup leftover) must not make the provider look
/// configured — the empty string would ride the whole send path and fail
/// deep inside the HTTP call with an error shape no gate understands.
/// The UI save paths reject empty input and the OAuth adapters validate
/// their tokens, but the filter here is what actually holds the line:
/// it is symmetric over both backends and needs no caller cooperation.
fn resolve_env_var_key(service: &str, env_var: &str) -> Option<String> {
    keyring_lookup(service, env_var)
        .filter(|key| !key.trim().is_empty())
        .or_else(|| {
            std::env::var(env_var)
                .ok()
                .filter(|key| !key.trim().is_empty())
        })
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
    if let Some(key) = resolve_env_var_key(service, env_var) {
        return Some(key);
    }

    // Shared-account providers (opencode-go) accept the account key saved
    // under the parent gateway's variable when their own is unset.
    let fallback = API_KEY_FALLBACK_ENVS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, var)| *var);
    if let Some(fallback_var) = fallback
        && let Some(key) = resolve_env_var_key(service, fallback_var)
    {
        return Some(key);
    }

    None
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

/// Whether the provider is `bedrock` — the one registry entry whose base
/// URL is region-templated and whose endpoint family lacks the OpenAI-
/// compatible `GET /models` and embedding surfaces (see the gates in
/// `connector::openai_compatible::caller`).
pub(crate) fn is_bedrock(provider: &str) -> bool {
    provider == "bedrock"
}

/// Validate an AWS region name against the region grammar
/// (`^[a-z]{2,4}(-[a-z]+)*-[0-9]+$` — us-east-1, eu-west-2, us-gov-west-1,
/// cn-north-1, and the EU sovereign cloud's four-letter eusc-de-east-1, …).
///
/// The validation is a SECURITY gate, not cosmetic: the region is spliced
/// into the endpoint HOST, and the `Authorization: Bearer` header rides that
/// host. `AWS_REGION` is ambient state set by dotfiles, CI configs and repo
/// `.env` loaders — an unvalidated value like `evil.com/x#` would silently
/// redirect the bearer token to a foreign origin. Anything that does not
/// parse as a region falls back to the registry default (a connection error
/// in the user's real region beats silent token exfiltration).
fn is_valid_aws_region(region: &str) -> bool {
    static REGION_SHAPE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    REGION_SHAPE
        .get_or_init(|| regex::Regex::new(r"^[a-z]{2,4}(-[a-z]+)*-[0-9]+$").expect("static regex"))
        .is_match(region)
}

/// Resolve the live base URL for the `bedrock` provider from the standard AWS
/// region environment variables. The registry entry only pins a default
/// region (`us-east-1`); real workloads run in the user's own region, which
/// AWS signals through `AWS_REGION` / `AWS_DEFAULT_REGION`.
///
/// Returns the provider's registry URL unchanged for every other provider —
/// and for bedrock when no VALID region variable is exported or when the
/// ambient region IS the registry default (an unregistered provider name
/// yields an empty URL — unreachable through [`Connector::new`], which
/// rejects unknown names). An explicit `params.base_url` (connector
/// `with_base_url`) always wins over this resolution; the helper exists so
/// the *default* path follows the ambient AWS configuration instead of
/// silently calling the wrong region.
#[must_use]
pub fn resolved_provider_base_url(provider: &str) -> std::borrow::Cow<'static, str> {
    let Some(config) = get_provider(provider) else {
        return std::borrow::Cow::Borrowed("");
    };
    if !is_bedrock(provider) {
        return std::borrow::Cow::Borrowed(config.base_url);
    }
    // First VALID region among the two variables. A PRESENT but blank or
    // invalid `AWS_REGION` must not shadow a valid `AWS_DEFAULT_REGION`
    // (the same placeholder rule the API-key resolution applies).
    let region = ["AWS_REGION", "AWS_DEFAULT_REGION"].iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty() && is_valid_aws_region(r))
    });
    match region {
        // The registry URL is region-templated (`bedrock-runtime.{region}.
        // amazonaws.com` with `us-east-1` pinned); re-point it to the ambient
        // region by name substitution. `us-east-1` itself needs no rewrite.
        Some(region) if region != "us-east-1" && config.base_url.contains("us-east-1") => {
            std::borrow::Cow::Owned(config.base_url.replace("us-east-1", &region))
        }
        _ => std::borrow::Cow::Borrowed(config.base_url),
    }
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
