# `connector::provider` — registry and API keys

The provider registry: which backends exist, their base URLs, default models,
and how API keys are resolved and cached.

## The registry

Each provider has a [`ProviderConfig`]:

```rust,ignore
pub struct ProviderConfig {
    pub name: &'static str,
    pub family: Family,              // OpenAICompatible | Gemini | Claude
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub needs_extra_headers: bool,   // e.g. OpenRouter
}
```

~26 providers are registered: `openai`, `groq`, `mistral`, `together`,
`openrouter`, `xai`, `deepseek`, `perplexity`, `fireworks`, `cohere`,
`huggingface`, `sambanova`, `poe`, `cerebras`, `nvidia`, `anyscale`,
`vercel`, `cloudflare`, `azure`, `ollama`, `lmstudio`, `vllm`, `llamacpp`,
`gemini`, `claude`, `zai`, `charm`. The local model servers (ollama,
lmstudio, vllm, llamacpp) point at `http://localhost:*`.

## Enumerating providers

```rust,ignore
pub fn known_providers() -> impl Iterator<Item = &'static str>
pub fn known_providers_with_env() -> impl Iterator<Item = (&'static str, &'static str)>
pub fn get_provider_env_var(provider: &str) -> Option<&'static str>
```

`known_providers_with_env` pairs each provider with its key env var
(`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, …) — handy for UI
rendering.

## API key resolution

```rust,ignore
pub const COSH_SERVICE: &str = "cosh";   // default keyring service

pub fn get_api_key(provider: &str, service: Option<&str>) -> Option<String>
pub fn has_api_key(provider: &str) -> bool
pub fn detect_provider() -> Option<&'static str>

// Example: Check if a provider is configured
if has_api_key("openai") {
    println!("OpenAI is configured");
}

// Example: Get the API key (respects priority: keyring → env → explicit)
if let Some(key) = get_api_key("anthropic", None) {
    println!("Found Anthropic key");
}

// Example: Detect which provider the user is most likely using
if let Some(provider) = detect_provider() {
    println!("Default provider: {}", provider);
}
```

Resolution order:

1. **OS keyring** — the authoritative store for keys saved explicitly through
   the app (`keyring::Entry` under `cosh` by default). Preferring it over the
   environment means stale shell/`.env` exports can't shadow a key the user
   configured.

> **Why keyring takes priority:** Users who explicitly save keys through the app have made a deliberate configuration choice. Environment variables are often temporary (session exports, `.env` files for development) and can become stale. By preferring keyring, we ensure the user's intentional configuration isn't accidentally overridden by forgotten environment state.

2. **Environment variable** — per-provider fallback for providers never
   stored (`get_api_key("openai", None)` reads `OPENAI_API_KEY`).

Successful keyring lookups are cached in a process-lifetime map (keyed by
`(service, env_var)`) so repeated resolutions don't re-hit the OS credential
store; misses are never cached, so keys added externally appear immediately.
Cached values are `Zeroizing` — wiped from memory on invalidation.

```rust,ignore
pub fn invalidate_api_key(env_var: &str)   // forget one provider's cached key
pub fn clear_api_key_cache()               // forget everything
```

Call `invalidate_api_key` after **storing or updating** a key, or the cache
serves the stale value.

- `has_api_key(provider)` — "configured?" regardless of backend (keyring or
  env).
- `detect_provider()` — the first provider (in registration order) that has
  a key; the natural default for "which provider is this user using?"

---

Next: [discovery — context windows and reasoning](discovery.md).

---

## Summary

- The provider registry contains ~26 providers with `ProviderConfig` entries: name, family (OpenAICompatible/Gemini/Claude), base URL, default model, and extra-headers flag.
- Providers include cloud services (openai, groq, mistral, together, openrouter, xai, deepseek, perplexity, etc.) and local model servers (ollama, lmstudio, vllm, llamacpp) pointing at localhost.
- Enumeration: `known_providers()` (iterator), `known_providers_with_env()` (pairs with env vars), and `get_provider_env_var()` (env var name for a provider).
- API key resolution: OS keyring (authoritative store under `cosh` service) → environment variable (fallback) → explicit `with_api_key` (highest priority).
- Keyring lookups are cached per process lifetime (keyed by service + env var) and `Zeroizing`; misses are never cached so externally-added keys appear immediately.
- Cache invalidation: `invalidate_api_key(env_var)` after storing/updating a key, or `clear_api_key_cache()` to forget everything.
- `has_api_key(provider)` answers "configured?" regardless of backend; `detect_provider()` returns the first provider with a key (natural default).
