# Append-only session history refactor

## Non-negotiable invariants

- [x] Every write to an existing session JSONL uses append mode only.
- [x] A session JSONL is the only authoritative record for metadata, display messages, context state, branches, revert, rollback, and deletion.
- [x] Every persisted state transition is a typed `Delta`; `Genesis` is the first delta of every new history.
- [x] Previously written bytes are never replaced, truncated, copied over, or removed by a session operation.
- [x] `Reference` identifies a reconstructable state in a history; branches and history actions store references rather than duplicated session snapshots.
- [x] A fork is a logical branch in its parent's JSONL and never creates or copies a JSONL.
- [x] Revert and rollback append deltas; original events remain reachable in history.
- [x] Snapshots, caches, summaries, and display/context structures are projections only and can be rebuilt from deltas.
- [x] Legacy JSONL remains readable and can receive append-only deltas without migration rewrites.

## Lifecycle and violation inventory (analysis complete)

- [x] Session creation currently exists only in memory until a valid dialogue save, then `write_session` creates a mutable header plus full message/item snapshots.
- [x] Normal, incremental, stopped, error, model-selection, session-switch, and title saves all route to `persist`/`write_session`, which atomically replaces the entire JSONL.
- [x] Header metadata (`title`, `title_generated`, `provider`, `model`, `reasoning`) and context bookkeeping (`next_id`, token budget, overflow, split, visibility boundary, hidden IDs) are rewritten on every save.
- [x] Display messages are rewritten as full-file projections; streaming changes and `ctx_ids` updates replace earlier records rather than record transitions.
- [x] Context items append in memory except for closure promotion; visibility, hidden IDs, overflow, split staging, token budget, and counters mutate snapshot fields and are persisted by header replacement.
- [x] `load_ctx_filtered` physically filters projected context for revert/fork; subsequent save replaces the history with the filtered state.
- [x] Revert truncates display messages, snapshots the whole file to `/tmp`, filters context, and rewrites the live JSONL.
- [x] `/undo` copies a saved JSONL over the live file, then queues another full rewrite.
- [x] Fork clones/truncates the session and context into a new session ID and a new JSONL, duplicating state.
- [x] Session deletion and retention eviction remove JSONL files, which is incompatible with immutable history.
- [x] Loads treat line 1 as a special header and body records as the latest complete snapshot rather than projecting deltas.
- [x] `ContextManagerState` is the runtime/harness transfer snapshot; it must remain a derived value while persistence converts state differences into deltas.
- [x] No distinct persisted `Hidden`/`Sample` types currently exist; hidden visibility and split/compaction staging are covered by context-state deltas, with the schema remaining extensible for future state variants.

## Phase 1 — Event schema and pure projection engine (complete)

- [x] Define versioned event-envelope, `Delta`, `Reference`, branch selector, metadata/message/context mutation, snapshot marker, revert, rollback, fork, and tombstone types in `session_history`.
- [x] Make `Genesis` a regular delta containing only initial state needed to begin one history/branch.
- [x] Implement a pure replay projection that derives all logical branches, current heads, session metadata/messages/`ctx_ids`, and `ContextManagerState` from ordered events.
- [x] Implement reference resolution for exact event states and message-prefix selections, including visibility-marker repair when a selected prefix excludes a compaction anchor.
- [x] Add legacy parsing as a synthetic immutable base projection; do not rewrite or discard legacy bytes.
- [x] Add focused schema/replay tests for genesis, mutations, references, revert, rollback, fork inheritance, tombstones, corrupt tails, and legacy input.
- [x] Validate with targeted session-store and context tests.

## Phase 2 — Strict append writer and snapshot-to-delta synchronization (complete)

- [x] Replace full-file serialization/atomic rename with a single FIFO append writer that assigns monotonic event IDs and appends complete newline-delimited deltas.
- [x] On first persistence, append `Genesis`; on later persistence, diff the requested `Session`/optional `ContextManagerState` against the replayed branch projection and append only typed mutations.
- [x] Cover metadata/title/model/reasoning, message create/update/remove, `ctx_ids`, context item append/replace, `next_id`, token budget, overflow, split, visible boundary, and hidden-set changes.
- [x] Ensure display-only saves preserve context by simply omitting context mutations.
- [x] Make load/list/summary/has-session resolve projected logical branches, including branches stored in another session ID's history file.
- [x] Replace physical delete and retention eviction with append-only logical tombstones; keep histories on disk.
- [x] Add byte-prefix invariant tests proving every save/title/model/context/delete path leaves all prior bytes identical.
- [x] Validate targeted tests, formatting, and compile checks.

## Phase 3 — Reference-based revert, rollback, snapshots, and fork (complete)

- [x] Replace `load_ctx_filtered` and destructive revert persistence with a `Revert` delta referencing the selected pre-message state/prefix.
- [x] Represent undo versions as persistent history references created by revert events; remove `/tmp` JSONL copies and restore-overwrite behavior.
- [x] Make `/undo` append `Rollback` pointing at the chosen pre-revert reference, then reload the derived branch view.
- [x] Make fork append one `Fork` delta to the parent's history with a parent/base reference and new branch metadata; never create a child JSONL or duplicate complete state.
- [x] Update app cache/sidebar/session switching so logical branch IDs load from their shared history and refresh after head changes.
- [x] Preserve prompt restoration, titles, model selection, context/message prefix semantics, and working-state guards.
- [x] Add tests proving revert/rollback/fork leave the physical file prefix intact, retain original history, reconstruct exact views, and keep one JSONL.
- [x] Validate message-action, undo, session-store, and app tests.

## Phase 4 — Context-manager delta semantics and persistence integration audit (complete)

- [x] Expose/centralize context-state comparison or mutation helpers so every persisted context change has an explicit context delta rather than opaque header bookkeeping.
- [x] Verify closure promotion, useless-chain/abandoned-input hiding, compaction boundaries, split begin/advance/commit/abort, overflow/model changes, and max-token changes round-trip through replay.
- [x] Treat harness `ContextManagerState` and emitted snapshots as transport/projection objects only; document that they are not authoritative persisted snapshots.
- [x] Audit every session-store caller and every filesystem mutation for bypasses; remove obsolete header/filter/copy/atomic-rewrite code and stale comments.
- [x] Add reconstruction tests spanning incremental saves, compaction, hidden state, interrupted split, resume, revert across compaction, fork, and rollback.
- [x] Validate context, harness, session-store, and TUI tests.

## Phase 5 — Final invariant validation and cleanup (complete)

- [x] Add a repository-level regression test or test helper that records file bytes before each supported operation and asserts the old bytes remain an exact prefix afterward.
- [x] Verify one physical JSONL for a root session plus all forks and no `/tmp` session snapshots.
- [x] Run `cargo fmt --check` and the relevant Clippy/build checks.
- [x] Run the full test suite (and feature-gated checks where practical).
- [x] Re-scan for `write`, `rename`, truncate, copy, and remove operations targeting session JSONL files; document any non-session JSONL operations as out of scope.
- [x] Update module/type documentation to describe immutable event history, projections, references, and logical branches in English.
- [x] Mark all invariants complete only after the final audit passes.

## Final validation record

- [x] `cargo test --bin cosh --no-default-features`: 415 passed, 16 ignored.
- [x] Focused history/store tests: 9 history tests and 36 store tests passed.
- [x] `cargo clippy --bin cosh --no-default-features -- -D warnings` passed.
- [x] All changed Rust files pass targeted `rustfmt --check`; `git diff --check` passes.
- [x] `cargo test --workspace --no-default-features` was run. The changed application and context suites passed; the workspace ended with two unrelated `cosh-sdk` gateway tests that expected a missing-key error but reached their `127.0.0.1:1` network sentinel instead.
- [x] Repository-wide `cargo fmt --check` was run. It remains blocked by pre-existing formatting drift in `src/harness/context/todo_ctxt.rs`, `src/tui/app/mouse.rs`, `src/tui/app/tests/sidebar_mouse.rs`, and `src/tui/routes/session/question.rs`; those unrelated files were not reformatted.
- [x] Workspace-wide strict Clippy was run. It remains blocked by the pre-existing `clippy::let_unit_value` finding in `src/harness/test/agent_loop_test.rs`; the affected binary target passes strict Clippy.
- [x] Final mutation scan found only the append-mode writer in production session persistence. Other JSONL persistence (`usage.jsonl`) and the context error catalog are separate stores and out of scope.
