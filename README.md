# cosh

**English** | [Português](./docs/readme/README.pt-BR.md) | [Español](./docs/readme/README.es.md) | [Русский](./docs/readme/README.ru.md) | [中文](./docs/readme/README.zh-CN.md) | [日本語](./docs/readme/README.ja.md)

Cosh is a general-purpose coding agent for the terminal, with software engineering tools and *computer tools* that let it interact with graphical applications.

[Cosh demo](https://github.com/user-attachments/assets/d069f7f5-d517-4df4-ab36-0a3943f096fa)

Highlights:

- **Computer tools via accessibility tree** — the agent reads and operates graphical interfaces through a structured, compact representation, falling back to screenshots only when needed. Fewer images processed, fewer tokens consumed.
- **TUI built from scratch in Rust** (Ratatui), inspired by OpenCode's UX. Providers, models and operating modes are configured right in the interface — almost zero manual setup.
- **Independent crates** — the development tools live in crates decoupled from the application, reusable as libraries outside Cosh.

## Installation

Tested on Linux and Windows. macOS hasn't been tested yet — if anything fails, [open an issue](https://github.com/DiegoZaluski/cosh/issues) with as much detail as possible.

**Linux, macOS, Windows (Git Bash / WSL):**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
```

Optional variables: `COSH_BIN_DIR` (install directory), `COSH_VERSION` (specific version) and `COSH_VARIANT` (`slim`, the default, or `slim-embed`, with local RAG via fastembed).

After installing, run `cosh` in your terminal. If the install directory isn't on your `PATH`, the script prints the command to add it. If anything goes wrong during installation, open an issue — I'll be glad to help!

## Crates

| Crate | Description |
|---|---|
| [`cosh-tools`](./crates/cosh-tools) | Development tools and computer tools |
| [`cosh-tui`](./crates/cosh-tui) | Terminal interface (Ratatui) |
| [`cosh-sdk`](./crates/cosh-sdk) | SDK for building agents on top of cosh |
| [`cosh-recall`](./crates/cosh-recall) | Memory and context recall |

## Roadmap

- **Laya** — a small decision-making model to assist the harness, shipped as a feature in a separate release.
- **Graph-based harness orchestration** — a new session mode for coordinating multiple harnesses.

## License

Apache-2.0.
