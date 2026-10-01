# cosh

[English](../../README.md) | [Português](./README.pt-BR.md) | [Español](./README.es.md) | [Русский](./README.ru.md) | **中文** | [日本語](./README.ja.md)

Cosh 是一个运行在终端中的通用编码智能体（coding agent），除软件工程工具外，还配备了 *computer tools*，使其能够与图形应用程序交互。

<img width="560" height="315" alt="demo" src="https://github.com/user-attachments/assets/d746684d-ecc1-4ce7-ad0c-a0b18d9892b8" />

主要特性：

- **基于 accessibility tree 的 computer tools** — 智能体通过结构化、紧凑的表示形式读取并操作图形界面，仅在必要时才回退到截图。处理的图像更少，消耗的 token 更少。
- **用 Rust 从零构建的 TUI**（Ratatui），交互设计受 OpenCode 启发。Provider、模型和运行模式都可以直接在界面中配置 — 几乎零手动配置。
- **独立的 crates** — 开发工具被拆分为与应用解耦的 crates，可以作为库在 Cosh 之外复用。

## 安装

已在 Linux 和 Windows 上测试。macOS 尚未测试 — 如果遇到问题，请[提交 issue](https://github.com/DiegoZaluski/cosh/issues)，并尽可能详细描述。

**Linux、macOS、Windows（Git Bash / WSL）：**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
```

可选变量：`COSH_BIN_DIR`（安装目录）、`COSH_VERSION`（指定版本）和 `COSH_VARIANT`（`slim` 为默认，或 `slim-embed`，含基于 fastembed 的本地 RAG）。

安装完成后，在终端运行 `cosh`。如果安装目录不在你的 `PATH` 中，脚本会自动输出添加它的命令。安装过程中遇到任何问题，请提交 issue — 我很乐意帮忙！

## Crates

| Crate | 描述 |
|---|---|
| [`cosh-tools`](../../crates/cosh-tools) | 开发工具与 computer tools |
| [`cosh-tui`](../../crates/cosh-tui) | 终端界面（Ratatui） |
| [`cosh-sdk`](../../crates/cosh-sdk) | 用于在 cosh 之上构建智能体的 SDK |
| [`cosh-recall`](../../crates/cosh-recall) | 记忆与上下文召回 |

## 路线图

- **Laya** — 用于辅助 harness 的小型决策模型，将作为 feature 随单独版本发布。
- **基于图的 harness 编排** — 一种用于协调多个 harness 的新会话模式。

## 许可证

Apache-2.0.
