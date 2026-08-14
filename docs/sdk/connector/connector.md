# The `connector` module: unified LLM client

`connector` is a **single client for every LLM provider**. Build one
[`Connector`](client.md) with a provider name, configure it with a builder
API, and call [`chat`](client.md), [`stream_chat`](client.md), or
[`embed`](client.md) — the same code works for OpenAI, Claude, Gemini, and
every OpenAI-compatible backend.

```rust,ignore
let reply = Connector::new("openai")?
    .with_model("gpt-4o")
    .with_temperature(0.7)
    .with_api_key("sk-...")
    .chat("Explain Rust ownership")
    .await?;
```

## The one-interface idea

Providers differ wildly on the wire — OpenAI's `/chat/completions`,
Anthropic's `/messages` with its `x-api-key` header, Gemini's
`generateContent` with `generationConfig` — but the surface here is a single
set of methods. The `Connector` internally classifies each provider into one
of **three families**:

| Family | Providers (examples) | Wire protocol |
|---|---|---|
| `OpenAICompatible` | openai, groq, mistral, deepseek, ollama, openrouter, together, xai, … | OpenAI `/chat/completions` + `/embeddings` |
| `Gemini` | gemini | Google `generateContent` |
| `Claude` | claude | Anthropic `/messages` |

Switching providers is a one-line change — `Connector::new("claude")`
instead of `Connector::new("openai")` — with each family translating the
shared parameters onto its native format.

## What the module provides

| Page | Contents |
|---|---|
| [`client`](client.md) | The `Connector` builder and its methods (`chat`, `stream_chat`, `embed`, `list_models`, `tokens`, introspection) |
| [`params`](params.md) | The request vocabulary: `ChatMessage` + builders, `ToolDefinition`/`ToolFunction`, `ResponseFormat` |
| [`output`](output.md) | Response types: `ChatOutput`, `ChatStream`/`StreamChunk`, `LsOutput`/`ModelInfo` |
| [`error`](error.md) | `ConnectorError` and the context-window classifier |
| [`provider`](provider.md) | The provider registry, API-key resolution (keyring + env), provider detection |
| [`discovery`](discovery.md) | Context-window and reasoning-capability discovery (static table → network catalogs) |

## Keys and credentials

API keys resolve in this order (see [`provider`](provider.md)):

1. **OS keyring** (under the `cosh` service) — the authoritative store for
   keys saved through the app;
2. **Environment variable** — per-provider fallback (`OPENAI_API_KEY`,
   `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, …);
3. **Explicit** `with_api_key("...")` — overrides both for that connector.

`has_api_key(provider)` / `detect_provider()` answer "is this provider
configured?" and "which provider is the user most likely using?" without
distinguishing the storage backend.

## A note on the internals

The `claude`, `gemini`, `openai_compatible`, and `common` submodules are
`pub(crate)` implementation details — the family-specific wire code and the
SSE plumbing. You never touch them: everything routes through `Connector`.

---

## Example

A complete runnable walkthrough lives at
[`examples/connector/client.rs`](../../../crates/cosh-sdk/examples/connector/client.rs):
it inspects the provider registry, builds and introspects connectors,
constructs tool/message payloads, exercises the deterministic error paths,
and makes best-effort live calls when an API key is available.

Next: [client — the Connector](client.md).
