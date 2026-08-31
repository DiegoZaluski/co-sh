# LSP Tools Reference

Language-server tools for the cosh agent. All tools follow the same
conventions as `fs` and `find`: a shared-state wrapper (`Lsp`), free-function
engines, and JsonSchema-annotated input/output types.

## Available tools

| Tool | Purpose |
|---|---|
| `lsp_diagnostics` | Settle + render diagnostics for a file or workspace |
| `lsp_definitions` | Go to definition (hybrid: position or symbol name) |
| `lsp_references` | Find references, grouped by file |
| `lsp_symbols` | Document symbols with hierarchy and kinds |
| `lsp_hover` | Type signature / documentation at a position |
| `lsp_workspace_symbols` | Project-wide symbol search via server index |
| `lsp_rename` | Two-phase rename (plan → confirm → apply) |
| `lsp_call_hierarchy` | Incoming/outgoing calls around a symbol |
| `lsp_restart` | Restart one or all language servers |

## Addressing model

Most tools use **hybrid addressing**: pass either an explicit 1-based
`position`, or just the `symbol` name and the first whole-word match in the
file is located automatically. This reduces round-trips when the model
already knows the symbol but not its exact column.

All coordinates in tool output are 1-based (line and character), matching
what models naturally produce.

## Rename safety

`lsp_rename` is two-phase by design:

1. **Dry-run** (default): returns files, edit counts, and preview lines.
   Nothing is written.
2. **Apply** (`confirm: true`): splices edits into each file back-to-front,
   aborting on overlap before writing anything.

Always run `lsp_diagnostics` after applying to catch fallout.

## Passive injection

The session layer attaches errors-only diagnostics to `fs_write` results as
a `<system-reminder>` block. This is the primary feedback loop — tools like
`lsp_diagnostics` are for explicit queries when the passive check isn't
enough.

## Discovery

Servers are discovered from a built-in catalog (~26 entries) by file
extension or exact extensionless file name (e.g. `Dockerfile`) plus
root-marker walk-up. Matching is strict: only an exact extension (`.rs`,
`.dockerfile`) or a case-insensitive exact name (`dockerfile`) claims a
spec — no loose heuristics. Binaries must be on `PATH`; missing ones
soft-skip without blocking other servers for the same file.

## MCP wiring

`cosh-server` exposes all tools over MCP. The wrapper binds to the process
working directory; opt-out via `COSH_LSP=off`.
