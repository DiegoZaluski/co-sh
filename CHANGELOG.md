# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2025-09-10

### Added

- Initial release of cosh, a coding agent for the terminal.
- TUI with syntax highlighting, markdown rendering and streaming agent responses.
- Built-in tools for file editing (with AST-aware edits), search (glob/grep),
  bash execution, web access, planning, skills and subagents.
- `cosh-sdk` — SDK crate with file search, diffing, patching and tree-sitter utilities.
- `cosh-tools` — tool implementations exposed to the agent.
- `cosh-recall` — session memory/recall with optional local embeddings (`fastembed`)
  and vector storage (`lancedb`).
- `cosh-tui` — reusable TUI toolkit on top of ratatui.

[Unreleased]: https://github.com/DiegoZaluski/cosh/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/DiegoZaluski/cosh/releases/tag/v0.1.0
