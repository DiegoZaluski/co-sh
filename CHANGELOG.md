# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Added

- TUI: scroll-to-bottom pill — when the user scrolls away from the live
  transcript, a small `↓` button appears just above the prompt box; clicking
  it jumps back to the latest content and re-engages sticky auto-follow.

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

[Unreleased]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/DiegoZaluski/co-sh/releases/tag/v0.1.0
