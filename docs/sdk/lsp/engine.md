# LSP Engine

The `cosh_sdk::lsp` module is a compact, tokio-native LSP client stack for
driving language servers from an agent process — no editor UI involved.

## Layering

Four layers, each testable in isolation:

| Layer | Module | Responsibility |
|---|---|---|
| Framing | [`jsonrpc`](#jsonrpc) | Content-Length framing + message classification |
| Transport | [`transport`](#transport) | I/O tasks, request routing, timeouts, backpressure |
| Client | [`client`](#client) | Handshake, document sync, server→client dispatch |
| Manager | [`manager`](#manager) | Discovery, lazy spawning, fan-out, lifecycle |

A fifth concern — diagnostics aggregation — lives in
[`diagnostics.rs`](diagnostics.md) and sits between the manager's event
stream and the tools.

## Design principles

1. **Behavioral port, not vendor.** The engine re-expresses battle-tested
   designs from Zed (`crates/lsp`), Helix (`helix-lsp`) and opencode (TS)
   using our own architecture on tokio + `lsp-types`. No GPL code, no gpui,
   no coupled crates.
2. **Disk is the source of truth.** No editor buffer: documents are opened
   from disk via `touch_file`, versions are internal counters, and stale
   content never reaches a server. The documents lock is held across the
   wire notifications, serializing every toucher of one path — a `didChange`
   can never overtake the `didOpen` it depends on, and two concurrent first
   touches cannot both send `didOpen`. The LRU slot is claimed before any
   wire send so concurrent observers see the pending open; if the send fails
   (session dying) the slot is rolled back for a clean retry.
3. **Every failure degrades cleanly.** Timeouts, stream death, protocol
   violations and missing binaries all surface as typed errors — nothing
   panics, nothing hangs. The transport re-checks the terminal exit watch
   while holding the pending map lock before issuing a request, so a request
   against a dead session fails fast with `NotRunning` instead of waiting
   out its deadline on a response that will never come.
4. **Backpressure over unbounded buffering.** Both the inbound (128 msgs)
   and outbound (256 frames) queues are bounded; when full, OS pipes apply
   pressure to the server instead of growing our heap.

5. **Canonical keys.** Project roots returned by the manager are
   canonicalized (with Windows verbatim-prefix stripping) so the same
   directory reached through a symlink or a non-canonical workspace root
   produces one `ClientKey` — never two server processes for one project.

## Key types

```text
Transport          Handle to one running connection (I/O tasks).
LanguageServer     Transport + handshake + doc sync + dispatch loop.
Manager            Workspace-level engine: discovery, lazy spawn, fan-out.
DiagnosticsEngine  Push+pull store with settle-wait and LLM formatting.
```

## Testing

All layers are tested over in-memory duplex pipes using the fake server
harness in `test_support.rs` — no real processes needed for CI.

## See also

- [Diagnostics](diagnostics.md) — push/pull hybrid store
- [Tools reference](../../tools/lsp/tools.md) — model-facing API
