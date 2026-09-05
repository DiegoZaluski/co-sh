# MCP client module (`src/mcp`)

Professional replacement for the provisional prototype in `src/harness/core.rs`
(`ServerSession`, `Harness::connect`, Tier 2 of `dispatch_next`).

Scope (agreed): tools-first, `stdio` + Streamable HTTP, no OAuth, config in
`setup.json`, footer-only status (count of connected servers). Resources,
prompts, tasks, subscriptions and a detailed panel are explicit follow-ups.

## Dependency policy

- Target: `rmcp 3.2.0` (latest stable; re-check with `cargo search rmcp`
  before release bumps). Requires Rust 1.88+.
- Reuse workspace crates only: `tokio` (full), `serde`/`serde_json`,
  `thiserror` and `url` (already in `cosh-recall`, `cosh-sdk`,
  `cosh-tools` at the same versions). No new crate without checking
  `Cargo.toml` + `crates/*/Cargo.toml` first.
- Client call-sites are source-compatible via `ServiceExt::serve`
  (legacy `initialize` lifecycle): `TokioChildProcess::new(cmd)`,
  `StreamableHttpClientTransport::from_uri`,
  `peer_info()`, `list_all_tools()`, `RunningService::call_tool()` (now
  drives MRTR automatically, still returns `CallToolResult` on success).
  A `Task` response surfaces as `ServiceError::UnexpectedResponse` (we
  never declare the tasks capability): the manager maps it to
  `McpError::Call` like any other call failure.
- Server-side mocks break: `ServerHandler::call_tool` now returns
  `Result<CallToolResponse, McpError>` (wrap with `.into()`); protocol
  union enums are `#[non_exhaustive]` (add wildcard arms).

## Contract freeze (Phase 0, done during planning)

- Prototype: `ServerSession`, `Harness::connect`, header building,
  extractor building, native-tools init, Tier 2 of `dispatch_next`
  (all in `src/harness/core.rs`).
- Consumers: `Harness::new` call sites in `src/tui/app/agent_loop.rs` and
  `src/tui/app/commands.rs` build a harness but never call `connect`.
- Tests: `src/harness/test/dispatch.rs` (`push_session`, in-memory
  `ServerHandler` mocks); `push_tool_call` stays.
- TUI: `AppState::{mcp_count, mcp_errors}` (`src/tui/state.rs`),
  footer rendering (`src/tui/routes/session/footer.rs`, no change needed),
  `Setup` (`src/tui/util/setup.rs`, no `mcp` section yet),
  `src/tui/routes/settings.rs` (pattern: hooks sub-lists).
- Example entry (not committed, for review):
  `stdio: { command: "npx", args: ["-y", "@modelcontextprotocol/server-everything"] }`,
  `http: { url: "http://localhost:8000/mcp" }`.

## Phase 1 — Upgrade rmcp 1.8 -> 3.2 isolated

- [x] `Cargo.toml`: `rmcp version = "3.2.0"`, keep features
      `client, server, transport-child-process,
      transport-streamable-http-client-reqwest`; `cargo update -p rmcp`
- [x] Migrate `src/harness/test/dispatch.rs` mocks to `CallToolResponse`
      (+ `.into()`), fix `#[non_exhaustive]` matches, `Meta` splits if hit
- [x] `cargo test --lib --no-default-features` + `cargo test --bin cosh
      --no-default-features` (dispatch subset first), `clippy -D warnings`
- [x] Review gate: compiler errors only from expected mock drift, no
      production behavior change

## Phase 2 — `src/mcp` skeleton (no wiring)

- [x] `src/mcp/mod.rs`: public surface + scope docs
- [x] `src/mcp/config.rs`: `McpServerEntry { name, transport, enabled }`,
      `Transport::{Stdio{command,args,env,cwd}, Http{url,headers,timeout_ms}}`,
      serde defaults, validation (`validate_mcp_entry`)
- [x] `src/mcp/error.rs`: `McpError` via `thiserror` (reused crate)
- [x] `src/mcp/types.rs`: `ServerStatus`, `ServerSnapshot`
- [x] `src/mcp/bridge.rs`: rmcp `Tool` -> `ToolSchema`/`ToolDefinition`/
      header text; `CallToolResult` content blocks -> `String`
- [x] Unit tests: config round-trip, validation, bridge conversions
- [x] Review gate (subagent 1)

## Phase 3 — `McpManager` (stdio + HTTP via `ServiceExt::serve`)

- [x] `src/mcp/manager.rs`: `HashMap<String, ManagedServer>`,
      `connect_one/connect_all/disconnect`, `status_snapshots()`,
      `aggregated_tools()`, `call_tool()` with per-call timeout
      (`tokio::time::timeout` over `RunningService::call_tool`)
- [x] Single-flight connect, per-server failure isolation (one server down
      never aborts the loop), `graceful_shutdown` on drop/disconnect
- [x] Tests over in-memory duplex (pattern from `dispatch.rs`) + timeout +
      unknown-tool routing
- [x] Review gate (subagent 2)

## Phase 4 — Harness swap (remove prototype)

- [x] Replace `sessions: Vec<ServerSession>` + `protocol` + `connect()` with
      `mcp: McpManager`; delete `ServerSession` (migrate `push_session`
      test helper to manager-backed injection)
- [x] Route header/extractor/native-tools/`dispatch_next` Tier 2 through
      the manager; keep `disabled_tools` + `Mode::Ask` filtering.
      `is_error` results propagate as `Err(text)` so tool-reported
      failures feed `correction_memory` instead of rendering as success.
- [x] Boot from `Setup.mcp` in `agent_loop.rs` before
      `format_header_context`; nested subagent inherits nothing (documented)
- [x] Full harness suite green
- [x] Review gate (subagent 3)

## Phase 5 — Config + Settings + footer

- [x] `Setup.mcp: McpConfig { servers }` with `#[serde(default)]`
      (legacy files load as empty); `save`/`load` round-trip test
- [x] `settings.rs`: `MCP servers` category (toggle + sub-list + Add,
      mirrors hooks); `validate_mcp` mirrors `validate_hook`
- [x] `HarnessEvent::McpStatus { servers }` at loop start + after
      connects/call failures; `agent_loop`/`events` reduces to
      `mcp_count` (ready) / `mcp_errors` (failed); drop demo `mcp_count = 3`
- [x] Review gate (subagent 4)

## Phase 6 — Final QA

- [x] Workspace tests + `clippy --lib --bin cosh -- -D warnings` +
      `rustfmt --check` + `git diff --check`
- [ ] Manual: one stdio + one HTTP server, one failing server isolated,
      restart, `disabled_tools` respected
- [x] Wire `disconnect_all` at agent-loop end (bounded `cancel` per client)
- [x] Record follow-ups: resources/prompts/tasks, subscriptions, OAuth,
      detailed panel, subagent MCP inheritance

## Non-goals this session

- Detailed per-server panel (footer count only).
- OAuth / `AuthorizationRequest`, SSE legacy, `Discover` lifecycle,
  `ClientCacheConfig` tuning.
- `mcp.json` separate file (deferred; `setup.json` is the single source).
