# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Amazon Bedrock as a provider. The `bedrock` entry joins the connector's
  OpenAI-compatible family through Amazon Bedrock's official OpenAI-compatible
  endpoint on `bedrock-runtime`: streaming, native tool calling, reasoning
  effort, and image input all work through the shared family path with no new
  dependencies. Auth rides the standard bearer path with the Bedrock API key
  (`AWS_BEARER_TOKEN_BEDROCK`); the endpoint is region-scoped, so the base URL
  follows the ambient `AWS_REGION` / `AWS_DEFAULT_REGION` automatically (with
  an explicit `with_base_url` always winning), and the region value is
  validated against the AWS region grammar before it ever touches the endpoint
  host — a malformed or hostile `AWS_REGION` falls back to the default region
  instead of silently re-pointing the bearer token elsewhere. AWS error bodies
  (`{"__type": "ThrottlingException", …}`, flat or wrapped) now classify
  through the shared error path, with throttling/service-outage kinds marked
  retryable so the existing backoff middleware paces them. The two surfaces
  the OpenAI-compatible endpoint genuinely lacks — model listing (`GET
  /models` exists only on `bedrock-mantle`) and embeddings (Titan/Cohere are
  Invoke-only) — fail up front with errors that explain the workaround
  (point `with_base_url` at the Mantle catalog, or use a proxy). Sixteen new
  tests cover the registry wiring, auth, streaming, tool calls, error
  classification, the gates, and the region resolution.

- The decision to NOT take on the full `aws-sdk-bedrockruntime`: the
  OpenAI-compatible endpoint covers everything cosh needs today, and the SDK
  would pull dozens of crates into the slim build. Users who later need the
  native Bedrock surface (Converse, Guardrails, Titan embeddings) will get an
  opt-in `bedrock-sdk` feature gate wrapping the SDK — the integration stays
  basic and light by default.

## [0.1.3] - 2026-10-04

### Added

- OAuth 2.0 device sign-in for the GitHub Copilot and Charm providers: sign
  in from within the interface and the token lands in the OS keyring, ready
  to use.

## [0.1.2] - 2026-10-03

### Added

- Free gateway with OAuth 2.0 sign-in. When a message fails for missing
  credentials — or a provider key is broken, revoked or out of quota — cosh
  offers its free gateway once, right in the session. Opting in runs the
  Charm device sign-in entirely in the interface: the recommendation dialog
  becomes the sign-in screen with the verification URL and a drag-copyable
  device code, Esc dismisses the box in any phase without cancelling the
  flow (it finishes on its own with a toast), and the parked message replays
  only after the user picks a model — never under the picker with a default.
  Under the hood, a hardened in-house OAuth 2.0 client (Authorization Code +
  PKCE S256, loopback redirect, device grant, issuer validation, zeroized
  secrets, single-flight token renewal) joins `cosh-sdk`'s connector, ready
  to back other providers.
- In-app update announcement. When a newer release is detected, the session
  footer shows a `current → new` version hint whose version button updates
  cosh in place — the same install pipeline as the Home banner's button —
  and the announcement toast carries a copy button with the manual install
  command, so updating never requires retyping a curl one-liner.

### Fixed

- Tool output no longer leaks into the TUI's right panel through an orphaned
  schema update: with a background subagent turn running, a main-agent
  tool's live chunks, results and failures were grafted into the running
  subagent window (and the subagent's own completion could then be dropped).
  The panel's update/complete/fail paths are now scoped to the tool's own
  window, and the leak family is covered by regression tests.

## [0.1.1] - 2026-10-02

### Added

- Subagents can now run in the background, allowing the main agent flow to
  continue while the task remains available for progress tracking.

### Fixed

- Adjusted the Spark animation frame pacing so the sparkle starfield no longer
  appears stuck while the TUI continues to perform smoothly.
- Fixed a panic (`range start index … out of range for slice of length …` in
  the session view render) that froze the TUI whenever a message's cached
  height underestimated its real render: the render-cache fill loop indexed
  past the end of the terminal buffer. The loop is now clamped to the rows
  that actually fit, and clipped renders are no longer written to the cache
  (guarding against layout jitter on later frames). Covered by regression
  tests in `tui::routes::session::tests`.

## [0.1.0] - 2026-10-01

Initial release of **cosh** — a coding agent for the terminal.

### Agent core

- General-purpose coding agent with a streaming TUI session: live transcript,
  tool-call rendering, markdown with syntax highlighting and diff previews.
- Providers, models and operating modes are configured from within the
  interface — almost zero manual setup.
- Multi-provider LLM support through a unified connector (Claude, OpenAI and
  Gemini via native adapters, plus OpenAI-compatible endpoints), with
  mid-stream retry and context-window awareness.

### Tools

- File editing anchored on content-hash tags (`hashline` patch format) with
  all-or-nothing preflight, stale-tag recovery and per-target error warnings.
- AST-aware structural editing (`ast-grep` + `tree-sitter`) with
  metavariable pattern rewrites.
- Session rollback: every write records a file version; restore by hash,
  step back, or undo a restore.
- Sandboxed path guarding: all paths are denied until the project root is
  set, with write and read-only allow/blocklists.
- Streaming bash execution (PTY mode, timeouts, chunked stdout/stderr) with
  pre-spawn security validation.
- Search (glob/grep), LSP integration (definitions, references, renames,
  hover, call hierarchy, passive diagnostics feedback), web fetch/search,
  planning, skills and structured user questions.
- Computer tools: observe and drive native desktop applications through the
  OS accessibility tree (X11 and Wayland), with screenshots as fallback —
  fewer images processed, fewer tokens consumed.
- Subagents via the Agent Client Protocol, dispatching to external harnesses
  (Gemini, Claude, Codex, Cline, Devin, Goose, OpenCode, Kilo) or an
  internal nested harness.

### Crates

The agent's building blocks are published as reusable libraries:

- `cosh-sdk` — LLM connector, tolerant tool-call extraction, hashline patch
  engine, file rollback, embedded LSP client stack (26-server catalog) and
  tree-sitter/ast-grep code intelligence.
- `cosh-tools` — every tool exposed to the agent, usable standalone.
- `cosh-tui` — Ratatui widget/renderable toolkit (markdown, diff, layout,
  colors) with a SolidJS-style reactive layer.
- `cosh-recall` — optional RAG layer (behind the `embed` feature): local
  embeddings via fastembed or cloud providers, stored in LanceDB.
- `cosh-onnx` — ONNX decision engine (behind the `onnx` feature, not
  published to crates.io), including the `cosh-decisiond` daemon that shares
  one resident model across running instances.

### Distribution

- One-line installer for Linux, macOS and Windows (Git Bash/WSL) via
  `download.sh` / `download.ps1`, with prebuilt binaries for
  x64 and arm64.
- Two release variants: `slim` (default, no RAG) and `slim-embed` (local RAG
  via fastembed). Both boot straight into a session.
- Library crates are published to crates.io on tag push
  (`cosh-sdk` → `cosh-recall` → `cosh-tools` → `cosh-tui`).

### Platform support

- Linux and Windows are tested. macOS builds are published but untested —
  report any issues with details.

[Unreleased]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/DiegoZaluski/co-sh/releases/tag/v0.1.1
[0.1.0]: https://github.com/DiegoZaluski/co-sh/releases/tag/v0.1.0
