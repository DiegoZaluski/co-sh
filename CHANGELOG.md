# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1]

### Fixed

- ACP: a goose pane no longer mixes old turns into a live one. Harnesses
  that advertise `loadSession` without `sessionCapabilities.resume` (goose)
  replay the retained conversation as ordinary `session/update` events
  before every pane turn, and those chunks were mapped like live traffic,
  polluting the pane's timeline and the turn's report. Replayed content is
  now dropped inside the session-open window, while pane chrome still
  passes and the rehydrate path keeps its unfiltered handler.

- Harness: report-owing turns recover from improper stops instead of
  ending with `incomplete`. The tool-loop detector's first firing grants
  one steering recovery, a natural stop with open todos resumes to
  reconcile the list first, and the checkup's natural-stop veto answers to
  a dedicated budget of three re-sends instead of `MAX_ITERATIONS`. Every
  accelerator re-checks the user's stop signal and is pane-scoped.

- The `/agents` context block shows each coworker's model, filled from the
  harness's last session report with the pane's committed `/models` pick
  as fallback, so the listing never guesses.

- Dialogs follow the mouse wheel: inside a dialog box it walks the scroll
  list a row at a time, and outside it both the scroll and the frame are
  left untouched.

## [0.2.0]

### Added

- Multi-agent orchestration under the cosh TUI: external ACP agents and
  nested cosh agents working side by side with the main chat, addressed and
  coordinated as one workspace.

  - The `/agents` split. A multi-select picker over the ACP harness
    registry (plus a synthetic cosh manager entry) divides the chat area
    into one pane per chosen agent, and a single shared prompt fans out to
    all of them at once. Every pane carries the session's interaction
    contract — per-pane scroll with sticky-bottom semantics, selection
    copy, focused dispatch, the chat's message actions, a mirrored
    sent-prompt box and the shared scroll-to-bottom pill — and the mode
    adds its own controls: a pane maximizes to take the whole area while
    the others keep running, hides with Backspace and restores with
    Ctrl+Z, and focusing a pane probes the agent's harness live to bring
    its own slash commands and session configuration boxes (model,
    provider, reasoning, mode) into the pane. The main agent mirrors
    itself in the Right Panel while the split is up, and adding or
    removing panes no longer kills the turns already running.

  - Persistent pane sessions and lifecycle. Each pane binds to its own
    resumable ACP session at the end of its first turn and rehydrates from
    the agent's session/load replay across app restarts (chat, reasoning
    and model pick included). The user's Build/Ask mode choice is mapped
    onto the closest advertised session mode at the start of every turn,
    each pane's ACP budget shows in its header, the focused pane's serving
    triple (provider, model, reasoning) shows on the prompt badge, and
    model selection is per pane — `/models` on a focused pane commits to
    that pane only, survives
    restarts through the session's own markers, and the last model picked
    for each agent is remembered across pane sessions, so a fresh pane no
    longer lands on the harness default (often a paid-tier model).

  - Coordination through the main agent. The main agent reaches the panes
    with the `agents_call` tool — blocking on the pane's report or parking
    it in the background with a requester-tagged receipt — and the
    `agents_compact` tool asks for one pane's context to be compacted
    ahead of the harness's 80% trigger. Every pane is told who it shares
    the workspace with through a peer preamble refreshed when the pane set
    changes; panes never coordinate themselves — coordination tooling is
    stripped from their loops and they only report.

  - Opt-in worktree isolation. Any pane — and the main agent itself — can
    work in its own git worktree instead of the shared checkout. cosh
    adopts existing worktrees and lists them, resolves explicit paths,
    refuses to run two agents in one checkout, quotes paths with spaces,
    and the main agent's isolation carries an exit path back to the shared
    checkout.

- Automatic hang detection for the bash tool (`bash_run`). A liveness
  watchdog samples the executed command's whole process tree — bash spawns
  descendants, so the tree, not just the shell, is watched — for three
  progress signals: new output, growing accumulated CPU time, and growing
  I/O counters (disk, and network on Windows). A command that shows none of
  the three for 120 seconds is presumed hung (waiting on a prompt, a lock or
  a dead connection) and killed, with a `liveness watchdog: ...` diagnostic
  on stderr. Any single progress signal resets the counter, so long-running
  but active commands — builds, downloads, imports — are never cut, no
  matter how long they run. An immediate escape complements the counter: a
  command that has already exited but whose orphaned descendants keep the
  output pipes/PTY open is detached at once and its own exit status is
  returned. This also catches hangs the raw-mode PTY watchdog never saw:
  canonical-mode prompts (`python input()`, `read -p`, password prompts).

- Process-tree kill on unix: every kill path (liveness watchdog, input-wait
  watchdog, hard runtime cap, zero timeout) now signals the child's whole
  process group with SIGKILL — the piped path spawns bash in its own
  process group and the PTY path already makes it a session leader —
  instead of killing only the shell. The old behavior leaked CPU-burning
  grandchildren that never touch the output pipes again (a leaked
  `yes >/dev/null` spins a core forever). On Windows the whole tree dies
  too: the killer walks fresh process snapshots from the root down and
  ends every descendant with a batched `taskkill` call (Windows has no
  process groups).

- An effective-context toggle that keeps the full model window. Discovered
  model windows are no longer always shrunk to the reduced sweet-spot
  budget: the new `effective_context` switch in Settings under Models and
  caching decides whether a discovered window is resized or used as-is, so
  a 1M model stays 1M unless the user opts into the reduction.

- Dynamic ACP and LLM provider registration. The built-in ACP harness
  registry gains GitHub Copilot CLI and Qwen Code, and users can register
  their own ACP agents (single-word name, launch line) and custom LLM
  providers (endpoint URL plus API family; OpenAI-compatible today) in
  `setup.json` through a registration form in Settings > Integrations.
  Registration is validated all-or-nothing, and a rotted entry cannot
  disable the working harnesses at startup.

- The subagent right-panel view reworked around per-call windows. Parallel
  calls of the same agent CLI no longer cross-pair: every subagent-bearing
  event carries a per-call routing identity, so each call's prompt echo,
  stream, report and failure land in its own window, and a straggler event
  for a completed window is dropped instead of spilling into a sibling.
  Windows read as chats now — the main agent's prompt echoes verbatim, the
  finished window seals its full transcript (reasoning, tool rows,
  narration) instead of being wiped, and edit diffs and tool names render
  for real. `subagent_call` takes a `description` shown in the list entry,
  and `Ctrl+Alt+L` toggles the list from anywhere.

- The computer toolset's last major surface: window management. The new
  `ComputerWindows` tool lists an app's windows (title, class, bounds,
  state) or performs one native verb — activate, minimize, maximize,
  fullscreen, restore, close, move or resize — through the OS window
  manager. Key holds land as standalone `hold`/`release` steps in the act
  and control pipelines, and waits gain `value`/`value_contains` (block
  until an element's value matches) and app-scoped event waits
  (window_opened, menu_closed, announcement…), replacing poll loops.

- Termination tuning: the decision checkup's confidence floor moves from
  0.6 to 0.75 — the measured optimum of a sweep over the termination corpus
  (23/24 correct, zero false continues) — and the harness now instructs the
  agent to open its final response with an explicit completion phrase, with
  a stop veto injecting a bare "continue" instead of coaching the model to
  finish remaining work (coaching confirmed a state the model could never
  disprove and looped forever).

- A `/spinner` picker: five styles for the agent-loop spinner — Bass
  (equalizer with a kick beat, the default), Runes, Opencode (direct port
  of the Knight-Rider scanner, 40 ms), Crush (direct port of crush's anim
  package, HCL gradient, 20 fps) and the classic braille wheel — persisted
  as `appearance.spinner` in `setup.json`. AnySpinner dispatches the shared
  surface, so the agent loop never learns the chosen style.

### Removed

- The model-facing `timeout_ms` argument of `bash_run`. Hangs are now
  detected automatically by the liveness watchdog, so the model no longer
  predicts and extends timeouts; the per-call override and its raise-only
  validation are gone from the schema and the tool description. A hard
  runtime cap remains as wrapper configuration (`Bash::timeout`, default
  raised from 10 to 30 minutes) as the last line of defense against
  commands that keep making progress forever (a CPU-burning busy loop is
  indistinguishable from real work); it emits a `runtime cap: ...`
  diagnostic when it fires.

## [0.1.4] - 2026-10-05

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

- Kimi (Moonshot AI) as a provider, through the connector's
  OpenAI-compatible family at `https://api.moonshot.ai/v1` with the
  `MOONSHOT_API_KEY`. The default model is `kimi-k3` (1M context,
  always-thinking with top-level `reasoning_effort`). Two Kimi quirks are
  handled in the shared path: streams that historically carry `usage` only
  inside each choice (top-level `usage` is tried first, the per-choice copy
  as a fallback), and cost reporting — Kimi's responses carry no per-request
  cost, so the session figure stays the reported sum while the account
  balance comes from the documented `GET /users/me/balance` endpoint, read
  defensively (a missing `data` reports the cost as unavailable instead of a
  silently wrong 0.0).

- Mouse-hover selection in the ADD Provider list and the /tools list,
  matching the model picker's behavior: moving the pointer over a row moves
  the highlight, and only the left-button release executes the action —
  hovering never toggles a tool or opens a provider credential dialog.

- Theme color swatches next to each theme name in the theme picker: a strip
  of the theme's identity colors (near-duplicates filtered out), so themes
  are told apart at a glance.

### Fixed

- Fixed the chat prompt's mouse copy selection dying when the drag left
  the prompt box. Drag ownership was re-derived per drag event from the
  current pointer row, so a drag anchored at the prompt's tail — the only
  direction that extends a selection from the end of the text — handed the
  gesture to the chat's drag extraction the moment the pointer dropped
  below the box; the release then copied the chat region and the prompt
  kept a zero-length anchor, making the tail impossible to copy without
  re-anchoring at the head or mid-text. A prompt-anchored press now owns
  its whole gesture: the drag keeps extending while the pointer is over
  the prompt rows, rows outside the visible text window clamp to the
  nearest text edge (standard editor drag semantics), and the release is
  consumed by the prompt's copy path even when it lands outside the box.
  The same anchor-ownership contract covers the right-button correction
  drag. Covered by gesture regression tests driven through the real mouse
  pipeline in `tui::app::tests::prompt_selection_mouse`.

- The `[Pasted ~N lines]` placeholder color no longer leaks into the
  text around it. The paste color was decided per wrapped line, so any
  line that merely overlapped the placeholder — ordinary text typed after
  it on the same line, or the line's wrapped continuation — was painted
  entirely in the placeholder color. The paint decision is now per glyph,
  driven by the exact byte ranges the placeholders occupy in the draft,
  and every occurrence of a duplicated placeholder gets its own range.
  Covered by cell-level regression tests in the prompt component.
- Fixed a panic (index out of range) that froze the TUI when pressing
  Ctrl+Up right after switching to a session with fewer messages: the
  prompt's history-navigation index survived the switch and indexed past
  the end of the shorter history. Both history navigation directions now
  clamp the carried index into range. Covered by regression tests in
  `tui::app::tests::history_panic_probe`.

- Setup settings no longer vanish during sessions. `setup.json` saves were
  a plain whole-file rewrite with no locking: two cosh processes running
  side by side performed a lost update (the last save silently discarded
  the other's changes to providers, mode selection and everything else), a
  crash or concurrent write mid-serialization could truncate the file, and
  any unparsable file was treated as missing — resetting the user to
  defaults without a trace. Saving is now a locked read-modify-write: an
  flock on `setup.json.lock` guards the section, what is currently on disk
  is read and merged under the in-memory settings (a section is kept from
  memory only when it differs from the save's own baseline snapshot, so
  sections this process never touched survive a concurrent writer), and
  the merged document is written to a temp file, fsynced and renamed into
  place so readers never observe a partial file. A corrupt `setup.json` is
  quarantined to `setup.json.corrupt-<timestamp>-<pid>` instead of silently
  reset, preserving the evidence for recovery, and the test suite no longer
  overwrites the developer's real config (an isolated config-dir guard
  covers the setup and uninstall tests).

- Sessions no longer vanish from the sidebar while another cosh process is
  still using them. The per-session flock was only claimed when switching
  to a session — every path that CREATED one assigned it directly, so a
  second process could adopt a brand-new session and both went on writing
  history to it concurrently. Session creation now goes through a single
  claim funnel that mints the id, takes the lock at creation time and only
  then hands the session over (/new, both agent-loop bootstraps and the
  fork action); a released claim is never re-minted by the original issuer,
  so two /new landing in the same millisecond cannot collide, and the lock
  follows the session through its lifecycle instead of being held until
  process death. Pressing Esc toward the Home screen is gated on the agent
  being Idle, so the hand-off can never persist a half-written session
  behind the running loop's back.

- Deleting or editing a queued message while the agent loop runs now
  actually reaches the loop. The old queued-input path was fire-and-forget:
  once the pump had pushed the head message the harness owned it, so a
  deleted message still reached the model on the next request, and editing
  the message the chain would send next paused nothing. The queue now
  lives in a shared retractable deque both sides hold by reference — the
  harness pops an entry only at the moment it injects it into the model
  context, a Delete retracts the pumped copy outright, an Edit retracts the
  stale text so the edited version is what gets injected, and editing the
  head of the next-agent-loop queue pauses the chain until the edit is
  submitted. All teardown paths (Esc stop, ClearQueue, session deletion,
  loop-end cleanup) clear the shared queue, so nothing parked can reach a
  dying loop and the next loop always starts fresh.

- Long lines in the session tool boxes are now wrapped instead of clipped:
  the bash output box, the glob file list, the write and read code panels,
  the todo list and the edit diff box all cut every source line at the box
  edge, and the height estimator counted raw source lines, so clipped boxes
  were also sized too short. One set of shared wrap/width helpers now sizes
  and paints every box, the code panels wrap without losing syntax
  highlighting (a token folded across two rows keeps a single color) and
  the estimated height always matches the painted rows.

- Direct answers no longer trigger a spurious continuation. The termination
  checkup audited every natural stop by sending the assistant's final text
  to the decision model alone, so a plain answer with no task wording ("2+2
  é 4") was judged as mid-task and the loop kept going after the work was
  already done. The agent loop now tracks whether any tool ran during the
  turn and only consults the checkup when it did: a cycle with no tool
  dispatch is a pure text exchange, answerable directly, with nothing to
  audit. A failed tool call still counts as agentic work, while a cycle
  whose tool calls were refused by the user lands unaudited — explicit user
  intent is never reviewed by the model. Enriching the decision state with
  the user's request was measured against the cached checkpoints and made
  things worse (every closing summary flipped to "running"); the negative
  result is recorded in the checkup module docs and kept as a standing
  evaluation probe.

- The footer's update button works again on Windows. The update pipeline
  built its cmd.exe one-liner (curl download, PowerShell installer,
  delayed-expansion exit code, relaunch) and dispatched it through
  `Command::args`: the standard library quotes arguments with the MSVCRT
  rules, but cmd.exe parses its command line with its own rules, where a
  backslash is literal — the backslash-escaped quotes therefore survived as
  literal quote characters, curl received an invalid quoted output path, the
  download failed and the `&&` chain short-circuited before the installer
  ever ran, all invisible under `CREATE_NO_WINDOW`, so the app closed and
  relaunched still on the old version. The one-liner is now passed with
  `raw_arg`, verbatim on the command line, and the installer's exit code is
  recorded with the `set "VAR=..."` idiom so the cleanup no longer appends a
  trailing space to the value. (The manual command copied from the update
  toast never went through this quoting layer, which is why it always
  worked.)

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

[0.2.1]: https://github.com/DiegoZaluski/co-sh/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/DiegoZaluski/co-sh/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/DiegoZaluski/co-sh/releases/tag/v0.1.1
[0.1.0]: https://github.com/DiegoZaluski/co-sh/releases/tag/v0.1.0
