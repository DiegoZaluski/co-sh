# co-sh

**English** | [Português](./docs/readme/README.pt-BR.md) | [Español](./docs/readme/README.es.md) | [Русский](./docs/readme/README.ru.md) | [中文](./docs/readme/README.zh-CN.md) | [日本語](./docs/readme/README.ja.md)

Cosh is a general-purpose coding agent for the terminal, with software engineering tools and *computer tools* that let it interact with graphical applications.

<img width="560" height="315" alt="demo" src="https://github.com/user-attachments/assets/d746684d-ecc1-4ce7-ad0c-a0b18d9892b8" />

Highlights:

- **Computer tools via accessibility tree** — the agent reads and operates graphical interfaces through a structured, compact representation, falling back to screenshots only when needed. Fewer images processed, fewer tokens consumed.
- **TUI built from scratch in Rust** (Ratatui), inspired by OpenCode's UX. Providers, models and operating modes are configured right in the interface — almost zero manual setup.
- **Independent crates** — the development tools live in crates decoupled from the application, reusable as libraries outside Cosh.

## Installation

Tested on Linux and Windows. macOS hasn't been tested yet — if anything fails, [open an issue](https://github.com/DiegoZaluski/co-sh/issues) with as much detail as possible.

**Linux & macOS** (also works on Windows via Git Bash / WSL):

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/co-sh/main/download.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/DiegoZaluski/co-sh/main/download.ps1 | iex
```

> Note: the `curl | bash` command does **not** work in PowerShell or CMD — Windows has no native `bash`. Use the PowerShell script above instead (Git Bash and WSL also work).

Optional variables: `COSH_BIN_DIR` (install directory), `COSH_VERSION` (specific version) and `COSH_VARIANT` (`slim`, the default, or `slim-embed`, with local RAG via fastembed). On PowerShell, set them as `$env:COSH_VERSION = "v0.1.0"` before running the command.

After installing, run `cosh` in your terminal. If the install directory isn't on your `PATH`, the script prints the command to add it. If anything goes wrong during installation, open an issue — I'll be glad to help!

## Crates

| Crate | Description |
|---|---|
| [`cosh-tools`](./crates/cosh-tools/README.md) | Development tools and computer tools |
| [`cosh-tui`](./crates/cosh-tui/README.md) | TUI widget library (markdown/diff rendering, layout) — the components the app's terminal interface is built on |
| [`cosh-sdk`](./crates/cosh-sdk/README.md) | SDK for building agents on top of cosh |
| [`cosh-recall`](./crates/cosh-recall/README.md) | Memory and context recall |
| [`cosh-onnx`](./crates/cosh-onnx/README.md) | ONNX decision engine (behind the `onnx` feature) |

## Roadmap

- **Laya/ONNX decision engine** — optional `onnx` feature backed by the `cosh-onnx` crate; see the [crate guide](./docs/onnx/onnx.md).
- **Graph-based harness orchestration** — a new session mode for coordinating multiple harnesses.

## License

Apache-2.0.
