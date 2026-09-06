# Layered context compaction refactor

## Summarization model settings

Implement only explicit user-authorized summarization routing. The ordered
`routing.summarization_models` list is independent of the agent's `auto` chain.
An empty list uses the active agent model. A nonempty list never silently adds
the agent model, built-in defaults, or an unselected provider as a fallback.
The review backlog is in ISSUES.md and is not part of this implementation.

### Phase 8 — Routing audit and implementation plan (complete)

- [x] Record pending review findings by phase in ISSUES.md, excluding the resolved byte-slicing issue.
- [x] Trace explicit versus auto selection, manual compaction, harness fallbacks, and internal subagents. No unrelated-model fallback was found for explicit TUI selections.
- [x] Trace Settings layout, keyboard/mouse actions, the searchable model dialog, model discovery, setup persistence, and all compaction entry points.
- [x] Record the manual-auto routing gap and include its resolution in the integration phase.

### Phase 9 — Persisted setting and Settings UX (complete)

- [x] Add a backward-compatible, empty-by-default ordered provider/model list.
- [x] Add a Settings block showing default behavior or numbered models, with add/edit, remove, and reorder controls.
- [x] Reuse searchable model discovery in an overlay without changing the agent selection or offering the `auto` pseudo-model.
- [x] Keep keyboard/mouse behavior aligned and make long lists usable in small terminals.
- [x] Test defaults, serialization, ordering, removal to empty, selection/cancellation, and agent-model isolation; TUI suite: 422 passed, 16 ignored.

### Phase 10 — Isolated summarization routing (complete)

- [x] Route automatic, manual, reactive-overflow, MapReduce map/reduce/audit/correction, legacy resume, and internal-subagent compaction through the configured list.
- [x] Try only configured models in order, stop on cancellation, preserve accepted progress, and surface exhaustion without silently using the agent model.
- [x] Keep the main agent connector, fallback chain, reasoning, and context-window accounting separate from the summarizer; size requests against the actual summarizer window while retaining the agent's checkpoint commit budget.
- [x] Preserve applicable cache preferences and configured local provider URLs; expose the summarizer identity and fallback attempts in UI notifications/usage.
- [x] Resolve manual `auto` via the user's auto chain when no summarizer is configured.
- [x] Add regression tests for explicit/default/auto routing, fallback order/exhaustion, cancellation/resume, model windows, and no unexpected model calls. Ten routing tests and an additional reactive-overflow test passed, including real local HTTP request/usage verification. Full library/TUI run before the final reactive test: 252 + 423 passed, 17 ignored.

### Phase 11 — Integration validation and handoff (complete)

- [x] Close the nested-harness visibility gap found during integration: forward usage and summarizer-route notices through the existing subagent bridge, without forwarding its context snapshots or changing the parent model. Add a bridge regression test.
- [x] Document the setting, ordering, defaults, failure behavior, and separation from agent auto routing.
- [x] Run library/TUI suites, strict Clippy, build, targeted formatting, and append-only lifecycle tests.
- [x] Record results and any remaining limitations; do not implement unrelated review issues or perform paid provider calls.

### Summarization settings validation record — 2026-09-06

- [x] Revalidated after the user's merges at `0e8b4ee`; preserved unrelated changes and the untracked session export.
- [x] `cargo test --lib --bin cosh --no-default-features --quiet`: library 289 passed, 1 ignored; TUI 444 passed, 16 ignored. Total: 733 passed, no failures.
- [x] `cargo clippy --lib --bin cosh --no-default-features -- -D warnings` and `cargo build --bin cosh --no-default-features` passed.
- [x] `cargo check --lib --bin cosh --no-default-features --features cloud` passed. Heavy optional embedding/vector backends were not enabled.
- [x] Targeted `rustfmt --edition 2024 --config skip_children=true --check` passed for all changed Rust files; `git diff --check` passed. No unrelated formatting was applied.
- [x] Local HTTP verified the selected summarizer model and configured local URL, no tools or inherited agent reasoning, bounded output, and correct usage identity. All provider responses were scripted; no paid/live-model evaluation was performed.
- [x] Regression coverage includes empty/default routing, explicit fallback order/exhaustion, cancellation, invalid/auto entries, reactive overflow, whole-item MapReduce fallback, retained accepted maps, correction/validation routing, legacy resume, manual-auto selection, and nested usage visibility.
- [x] Settings tests cover backward-compatible defaults, ordered persistence, search-result refresh without `auto`, agent-selection isolation, cancellation, mouse/keyboard reordering/removal, and scrolling long lists.
- [x] Existing session-store byte-prefix, checkpoint, fork, revert, rollback, and staging migration tests remain green. The global setting lives in setup.json; session histories are not rewritten.
- [x] Remaining review findings stay in ISSUES.md. Explicit summary models use provider-default reasoning; unknown windows retain the existing estimate until a provider reports a limit. Settings changes apply to newly created harnesses, not an in-flight turn.

## Correction — Semantic MapReduce boundaries

The grouping target is a soft packing budget, not the model's request window.
Never split an item to meet that target: move it intact to the next group,
allow an oversized singleton, and reset accounting for following groups.
Only a complete request exceeding the actual model window is an overflow.

### Phase 6 — Whole-item partitioning and regression tests (complete)

- [x] Replace byte slicing with ordered whole-item references and reset packing budgets at every group boundary.
- [x] Repartition incomplete groups only between items; keep accepted summaries and distinguish a reduced grouping target from the actual request window.
- [x] Version derived staging so legacy fragment summaries are rebuilt from immutable source, without rewriting history.
- [x] Add regressions for target overflow, subsequent groups, multibyte content, true oversized singletons, recovery, and legacy staging.
- [x] Update mock fixtures to use multiple intact items where multiple maps are required; validate focused tests before continuing. `cargo test --lib --no-default-features map_reduce -- --nocapture`: 26 passed.

### Phase 7 — Persistence, request-budget, and integration validation (complete)

- [x] Verify full map requests that exceed the grouping target still fit and are sent intact, and genuinely oversized requests fail without slicing or retry loops.
- [x] Verify persisted whole-item staging and legacy restart preserve the JSONL byte prefix.
- [x] Update compaction documentation to state the semantic-boundary policy and its true-window limitation.
- [x] Run library/TUI regression suites, strict Clippy, targeted formatting, and whitespace checks; record results and limitations.

### Correction validation record

- [x] `cargo test --lib --bin cosh --no-default-features --quiet`: library 242 passed, 1 ignored; TUI 419 passed, 16 ignored. No failures.
- [x] `cargo clippy --lib --bin cosh --no-default-features -- -D warnings` and `cargo build --bin cosh --no-default-features` passed.
- [x] Targeted `rustfmt --check` on all four changed Rust files and `git diff --check` passed. Unrelated workspace formatting was not changed.
- [x] Local HTTP transport verified exact full-prompt delivery for a whole item above the soft target and below the 4,000-token request window, including instructions and reserved output. An actually oversized item fails before network access, without slicing or committing.
- [x] Persisted legacy-fragment restart, whole-item resume, and the existing checkpoint/fork/revert/rollback lifecycle all preserve the prior JSONL byte prefix.
- [x] The repeated offline trace retains eight maps, ten scripted requests, and the same final view; peak map input is now 4,017 estimated tokens. No live-provider evaluation or model-recall guarantee is implied.
- [x] Oversized indivisible requests fail explicitly; no new semantic splitter was introduced. Previously committed checkpoints are not retroactively rebuilt. This correction invalidates only legacy derived MapReduce staging.

## Goal

Replace the greedy split-and-concatenate contingency with a recoverable layered
context policy:

`deterministic masking + structured state + recent raw window + referenced checkpoints + reset/handoff`

When one-shot compaction cannot fit the active model, use hierarchical
MapReduce: summarize independent event ranges, reduce the complete set of
range-addressed summaries, re-read raw ranges when the reducer reports a
conflict, and validate that decisions, constraints, and open work survived.

## Non-negotiable invariants

- [x] Session JSONL remains the sole authoritative history and strictly append-only.
- [x] Masking, checkpoints, MapReduce progress, reset/handoff, retries, and fallback decisions persist only as new deltas.
- [x] Raw context items remain reconstructable; masking and checkpoints affect only the derived model view.
- [x] Every checkpoint and mapped summary identifies the exact context item range it covers.
- [x] A recent high-fidelity window remains raw and chronological after automatic checkpointing, except when an oversized indivisible source item/tool chain leaves no room for a valid checkpoint. Manual compaction retains its existing whole-responded-history behavior; queued input remains raw.
- [x] Protected task state, user constraints, decisions, and open work cannot be silently discarded by deterministic masking.
- [x] A failed, interrupted, or partial compaction never changes the committed model view.
- [x] Legacy histories and legacy in-progress split state remain readable without rewriting existing bytes.
- [x] Provider concurrency failures fall back to bounded sequential mapping without losing successful segment results.
- [x] Existing behavior is preserved unless it conflicts with the layered context architecture.

## Lifecycle analysis and violation inventory (complete)

- [x] Context ingestion is centralized in `ContextManager`; user, assistant, tool call/result, closure, compaction, and display-only error items receive monotonic IDs.
- [x] The current deterministic pass hides only explicitly useless tool chains and abandoned/cancelled debris; useful historical tool results remain raw until LLM compaction.
- [x] `build_messages` currently projects every visible item in timeline order, then appends the protected live TODO block at the tail.
- [x] Automatic compaction runs before the first model request and after tool dispatch when estimated visible tokens reach 80% of the effective model window.
- [x] Manual `/compact` forces the same LLM path by temporarily setting the trigger to zero.
- [x] One-shot compaction serializes the entire visible projection, updates a previous compaction anchor when present, and hides every pre-existing item behind a new summary boundary.
- [x] Provider overflow in the main request or one-shot summarizer enters `split_context`; known window discovery and provider-reported windows size the contingency.
- [x] The current split staging is a growing concatenated buffer plus cursor and 1,200-character continuity tail; chunks are sequential, never globally reconciled, and a single oversized item is still sent whole.
- [x] Split progress is transport state persisted through `ContextDelta::Split`; the final anchor is committed only after all chunks succeed, but the harness does not currently emit a persistence snapshot after each chunk.
- [x] Generic summarizer errors use bounded retries; context-window errors abort the split, mark the active model stuck, and notify the user until a model switch or successful compaction clears it.
- [x] Fallback model switches rebuild the connector, tokenizer, tools, and window; an active split is resumed before retrying the main request.
- [x] Summarizer calls use the active connector without tools, share the stop signal, stream text into one TUI compaction box, and report provider usage after each completed call.
- [x] The append-only session store diffs `ContextManagerState` into item, visibility, hidden-set, overflow, split, and budget deltas; replay reconstructs branch-local context state.
- [x] Existing `Compaction` items are structured Markdown anchors but do not record their covered item range and always replace the whole visible projection rather than preserving a recent raw window.
- [x] Large bash/web outputs already use recoverable head/middle/tail truncation before entering context; masking must compose with those references instead of creating another authoritative store.
- [x] The live TODO block is already protected structured state, but the remaining objective/decision/open-work state exists only inside free-form compaction summaries.

## Phase 1 — Deterministic masking and referenced checkpoint composition (complete)

- [x] Define serializable item-range/checkpoint metadata and a persisted masked-result set with backward-compatible defaults.
- [x] Add deterministic masking for reacted-to historical tool results while preserving a configurable recent raw token window and valid tool-call/result pairing.
- [x] Render masked results as typed, range-addressable placeholders while retaining their original content in the authoritative history and in-memory projection.
- [x] Change compaction planning to select an old prefix for checkpointing while preserving recent raw items; include previous checkpoints in the selected source state.
- [x] Detect an oversized native tool chain in the raw reserve and checkpoint it whole rather than mutating signed call arguments or accepting a handoff that remains over budget.
- [x] Commit a checkpoint atomically with exact covered ranges, derive source visibility from the checkpoint, and compose it before the remaining raw tail regardless of append position.
- [x] Persist/replay masked IDs and checkpoint metadata through append-only context deltas, including legacy defaults and visibility repair.
- [x] Add focused tests for masking safety, raw-window preservation, chronological composition, checkpoint references, restore/replay, revert/fork compatibility, and byte-prefix immutability.
- [x] Validate context-manager and session-history/store tests; mark Phase 1 complete before starting Phase 2.

### Phase 1 validation record

- [x] `cargo test --lib --no-default-features`: 219 passed, 1 ignored.
- [x] `cargo test --bin cosh --no-default-features`: 416 passed, 16 ignored.
- [x] `cargo clippy --lib --bin cosh --no-default-features -- -D warnings` passed.
- [x] Targeted `rustfmt --check` and `git diff --check` passed.

## Phase 2 — Independent map segments and hierarchical reduction (complete)

- [x] Replace greedy split staging with versioned MapReduce staging while retaining a deserialization path for legacy `SplitState`.
- [x] Partition the selected checkpoint source into independent token-bounded segments without continuity tails; record exact item ranges and stable segment ordinals.
- [x] Make every map prompt produce a self-contained structured mini-summary containing its source range and explicit decisions, constraints, invalidations, completed work, open work, artifacts, and unresolved conflicts.
- [x] Store successful map outputs independently and idempotently so interrupted work resumes without regenerating completed segments.
- [x] Build a reducer request over all ordered mini-summaries; if that request cannot fit, recursively reduce bounded groups while preserving the union of their source ranges.
- [x] Add a reducer conflict protocol that can request bounded raw item ranges and rerun reduction with those excerpts.
- [x] Add a validation pass that audits the candidate against all mapped summaries and either accepts it or returns a corrected final checkpoint.
- [x] Commit only the validated final checkpoint; abort/interrupt leaves the prior view unchanged and all raw events reachable.
- [x] Add unit tests for range partitioning, ordering, interruption/resume, recursive reduction, conflict re-read, validation correction/failure, oversized single items, and atomic commit.
- [x] Validate focused context and harness tests; mark Phase 2 complete before starting Phase 3.

### Phase 2 validation record

- [x] `cargo test --lib --no-default-features`: 225 passed, 1 ignored.
- [x] `cargo test --bin cosh --no-default-features`: 416 passed, 16 ignored. One fork-title test failed in the first parallel run, passed alone, and passed in the full rerun.
- [x] `cargo clippy --lib --bin cosh --no-default-features -- -D warnings` passed.
- [x] Targeted Rust formatting and `git diff --check` passed.

## Phase 3 — Parallel mapping with sequential provider fallback (complete)

- [x] Stop scheduling new parallel calls after the first failure, drain already-running successes, and make fallback backoff and stalled streams interruptible.
- [x] Apply overflow repartitioning in sequential fallback too; retain accepted map identities and display only the committed checkpoint after validation.
- [x] Persist manual-compaction progress while the agent is idle; the existing snapshot handler only accepts working agent loops.

- [x] Add bounded concurrent map execution using cloned no-tools connectors and stable ordinal-based result placement.
- [x] Preserve cancellation, per-request usage reporting, retry classification, and deterministic test behavior under concurrency.
- [x] Detect provider/rate/concurrency failures, retain successful map outputs, and retry only incomplete segments sequentially with bounded backoff.
- [x] Treat a genuine per-segment context overflow as a repartition signal rather than a concurrency failure.
- [x] Emit incremental context snapshots after each accepted map/reduce result so resumable staging is actually durable.
- [x] Make TUI progress phase-aware without interleaving parallel mini-summary text into a misleading final-summary stream.
- [x] Add tests for out-of-order completion, partial parallel failure, sequential fallback, cancellation, usage events, persistence snapshots, and exactly-once segment acceptance.
- [x] Validate harness and TUI compaction tests; mark Phase 3 complete before starting Phase 4.

### Phase 3 validation record

- [x] Library tests: 230 passed, 1 ignored; TUI tests: 417 passed, 16 ignored.
- [x] Local HTTP fixture verified no-tools requests and provider usage/cost events.
- [x] Strict Clippy, targeted formatting, and byte-whitespace checks passed.

## Phase 4 — Structured handoff and lifecycle integration (complete)

- [x] Bound reduction levels and correction attempts, revalidate corrected candidates, and reject malformed control replies instead of committing them.
- [x] Verify source identity on resume and commit, preserve logical checkpoint-first ordering, and prevent partial rereads from claiming complete range coverage.
- [x] Allow conflict rereads through prior checkpoint references, reject unavailable raw ranges, and report exhausted reduction/correction stages explicitly.
- [x] Account for prompt overhead and output limits in checkpoint calls and make undersized-window failures explicit and bounded.

- [x] Refine the checkpoint schema/prompt into protected task state: objective, constraints, decisions with provenance, file/artifact references, verified work, active work, blockers, and next action.
- [x] Ensure stale facts can be explicitly invalidated and do not survive merely because they appeared in an older checkpoint.
- [x] Use the composed checkpoint plus recent raw tail as a clean handoff after automatic, manual, reactive-overflow, and fallback-model compaction paths.
- [x] Ensure queued user input remains verbatim and is never consumed solely into a checkpoint.
- [x] Update overflow/stuck semantics so single-shot, map, reduce, validation, and main-request failures choose the correct recovery path without retry loops.
- [x] Update event names, UI copy, module/type documentation, and tests from split/concatenate terminology to checkpoint/MapReduce terminology.
- [x] Add end-to-end tests spanning repeated checkpoints, model changes, manual compaction, provider overflow, fallback models, stop/resume, rollback, revert, and fork.
- [x] Validate context, harness, session-store, and TUI suites; mark Phase 4 complete before starting Phase 5.

### Phase 4 validation record

- [x] Library tests: 236 passed, 1 ignored; TUI tests: 418 passed, 16 ignored.
- [x] Strict Clippy, targeted formatting, and `git diff --check` passed.
- [x] The persisted lifecycle fixture proves byte-prefix preservation through partial maps, checkpoint commit, logical fork, revert, and rollback.
- [x] Existing manual compaction retains its whole-responded-history behavior; unanswered/queued user messages remain raw. Automatic checkpointing retains the recent raw reserve.
- [x] Structured Markdown and model audits improve handoff discipline but do not constitute a proof of factual recall; live-model quality remains a separate evaluation.

## Phase 5 — Final invariant and quality validation (complete; offline evaluation)

- [x] Add a reproducible offline long-trace comparison with scripted summaries, source-coverage checks, request/token accounting, and explicitly non-provider latency measurements.
- [x] Document that factual-quality and provider-latency measurements require a separate live-model evaluation; do not infer model recall from scripted responses.
- [x] Correct remaining legacy-only documentation discovered in the final terminology audit.
- [x] Investigate SDK gateway-test failures with safe environment-presence checks and isolated reruns; preserve unrelated SDK code.
- [x] Let an explicit model switch renew exhausted reduction/correction budgets without discarding accepted maps; same-model retries must remain bounded.
- [x] Record unrelated tools-suite PTY-length/path assumptions and workspace formatting failures; run all targets without fail-fast so they do not hide remaining results.
- [x] Replay long immutable traces comparing the old greedy contingency, masking-only, one-shot checkpointing, sequential MapReduce, and parallel MapReduce.
- [x] Measure scripted task-state retention, critical constraint/decision recall, stale-fact removal, open-work survival, token reduction, request count, local orchestration time, and recovery behavior; record live-model quality/cost/latency as unmeasured rather than extrapolating from mocks.
- [x] Cover adversarial traces: one huge item, repeated compactions, changed files after reads, repeated failures, conflicting summaries, and compaction across revert/fork/rollback.
- [x] Prove every supported compaction operation leaves all previous JSONL bytes as an exact prefix and creates no second authoritative state store.
- [x] Re-scan persistence and context code for truncation, overwrite, unreferenced summaries, destructive rollback, or view state that cannot be rebuilt from deltas.
- [x] Run formatting, targeted Clippy/build checks, full workspace tests, and feature-gated checks where practical.
- [x] Record validation results and any unrelated pre-existing failures here.
- [x] Mark every invariant complete only after the final audit passes.

### Phase 5 validation record

- [x] `cargo test --lib --no-default-features`: 237 passed, 1 ignored.
- [x] `cargo test --bin cosh --no-default-features`: 418 passed, 16 ignored.
- [x] `cargo build --bin cosh --no-default-features` passed.
- [x] `cargo check --lib --bin cosh --no-default-features --features cloud` passed. Heavy optional embedding/vector backends were not enabled; no claim is made about those feature combinations.
- [x] `cargo clippy --lib --bin cosh --no-default-features -- -D warnings`, targeted `rustfmt --check`, and `git diff --check` passed.
- [x] Final workspace run used `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent-cosh-test-bus cargo test --workspace --no-default-features --no-fail-fast --quiet`: 2,092 passed, 37 ignored, 1 unrelated failure. SDK: 586 passed; tools: 392 passed, 7 ignored, 1 failed; TUI crate: 455 passed. All doctest targets completed.
- [x] The remaining failure is `cosh-tools::fs::test::test_write::write_creates_multiple_files_in_single_call`, which asserts an absolute `¶/home/inky/co-sh/ftest.txt#` header. Its code is unchanged. The test-created `ftst2.txt` artifact was removed after verification; no unrelated files were changed.
- [x] An earlier tools run intermittently failed `test_spawn_bash_pty_large_output` (8,190 versus 8,292 bytes). It passed in isolation and in the final full run.
- [x] Two SDK gateway missing-key tests fail against the host keyring but pass with the process's D-Bus connection isolated; all nine gateway-focused tests and the full SDK suite passed under that isolation. No credential was read into tool output, and no SDK/authentication code was changed.
- [x] Workspace-wide `cargo fmt --all -- --check` still reports pre-existing differences in `src/harness/context/todo_ctxt.rs`, `src/tui/app/mouse.rs`, `src/tui/app/tests/sidebar_mouse.rs`, and `src/tui/routes/session/question.rs`. These unrelated files were preserved.
- [x] Persistence scan: production session writes converge on `append_events` using `OpenOptions::append(true)`; source-range summaries and staging stay in the same delta history. Remaining session-store `fs::write` calls construct test fixtures. `truncate` changes only in-memory branch projections. `error_catalog.rs` atomically updates the separate provider-window catalog, not session data.
- [x] The offline 24-turn trace starts at 29,337 estimated tokens: masking leaves 5,757; sequential and parallel MapReduce both leave 1,300 with eight maps, one reduction, and one audit. All five scripted task-state markers survive, stale content is excluded from the checkpoint view, and raw items remain intact.
- [x] `docs/context-compaction.md` records the architecture, recovery limits, reproducible five-path comparison, and explicit limitations. Scripted retention and local mock timings do not establish real-model recall, quality superiority, cost, or provider latency. No paid/live-provider evaluation was performed; the document specifies a separate evaluation protocol.
