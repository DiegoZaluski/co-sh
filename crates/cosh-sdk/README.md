# cosh-sdk

[![Crates.io](https://img.shields.io/crates/v/cosh-sdk.svg)](https://crates.io/crates/cosh-sdk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DiegoZaluski/co-sh/blob/main/LICENSE)

SDK crate for [cosh](https://github.com/DiegoZaluski/co-sh) — a coding agent for the terminal.

`cosh-sdk` consolidates the building blocks of a coding agent into a single library: a unified LLM provider client, tolerant tool-call extraction, a content-hash file patch format with session rollback, an embedded LSP client stack, and AST/code intelligence over 60+ languages. It is a plain library with no dependency on the cosh app — you can build your own agent on it alone.

## Highlights

- **One `Connector`, 28 providers** — Claude, OpenAI, Gemini (native adapters) plus OpenAI-compatible endpoints (DeepSeek, Groq, xAI, Ollama, LM Studio, vLLM, …). Streaming chat, tool calling, embeddings, token usage and cost tracking.
- **Streaming with mid-stream retry** — automatic retry on 429/5xx/network errors with backoff and `retry-after` support; a reset marker lets you discard partial text from a failed attempt.
- **Context-window intelligence** — model catalog with context-window discovery and `ContextWindowExceeded` classification, even from ambiguous SSE error bodies.
- **Tolerant tool-call extraction** — turns raw LLM output (streamed or batch, inline JSON or native tool calls) into validated tool calls, with rejection messages designed to teach the model to retry.
- **The hashline patch format** — content-hash-tagged multi-file edits with all-or-nothing preflight, stale-tag recovery, CRLF/BOM tolerance and streaming parse.
- **Session rollback** — bounded file-version history per session; restore by hash, step back, or undo a restore.
- **Embedded LSP stack** — tokio-native client with a curated 26-server catalog (rust-analyzer, gopls, pyright, clangd, …), lazy spawn, backpressure, and model-facing diagnostic formatting.
- **Code intelligence** — ast-grep structural search/rewrite across 60+ tree-sitter grammars, LRU-cached incremental parsing, and syntax highlighting.

## Usage

```rust
use cosh_sdk::connector::Connector;

let reply = Connector::new("openai")?
    .with_model("gpt-4o")
    .with_temperature(0.7)
    .with_api_key("sk-...")
    .chat("Explain Rust ownership")
    .await?;
```

Modules: `connector` (LLM clients), `extract_action` (tool-call parsing), `hashline` (patch engine), `rollback` (file versioning), `lsp` (language servers), `ast` / `tree_sitter` / `find` (code search and parsing).

More runnable examples in [`examples/`](https://github.com/DiegoZaluski/co-sh/tree/main/crates/cosh-sdk/examples) — run with `cargo run -p cosh-sdk --example connector` (also `hashline-patcher`, `rollback`, `find`, `ast-ops`, `tree-sitter`, `extract-action`).

## Requirements

- Rust 2024 edition (≥ 1.85)
- API keys via environment or the OS keychain (native keyring support)
- LSP servers installed on `PATH` separately (no auto-install)

Note: the tree-sitter grammar dependencies (~50 crates) are the most significant compile-time cost. If you only need the connector or the patch engine, compiling is faster on a stable toolchain cache.

## Related crates

- [`cosh-tools`](https://crates.io/crates/cosh-tools) — the reference tool implementations (fs, bash, web, computer control…)
- [`cosh-recall`](https://crates.io/crates/cosh-recall) — memory and context recall
- [`cosh-tui`](https://crates.io/crates/cosh-tui) — terminal interface

## License

Apache-2.0. See [LICENSE](https://github.com/DiegoZaluski/co-sh/blob/main/LICENSE).
