# Layered context compaction refactor

## Goal

Replace the greedy split-and-concatenate contingency with a recoverable layered
context policy:

`deterministic masking + structured state + recent raw window + referenced checkpoints + reset/handoff`

When one-shot compaction cannot fit the active model, use hierarchical
MapReduce: summarize independent event ranges, reduce the complete set of
range-addressed summaries, re-read raw ranges when the reducer reports a
conflict, and validate that decisions, constraints, and open work survived.

## Non-negotiable invariants

- [ ] Session JSONL remains the sole authoritative history and strictly append-only.
- [ ] Masking, checkpoints, MapReduce progress, reset/handoff, retries, and fallback decisions persist only as new deltas.
- [ ] Raw context items remain reconstructable; masking and checkpoints affect only the derived model view.
- [ ] Every checkpoint and mapped summary identifies the exact context item range it covers.
- [ ] A recent high-fidelity window remains raw and in chronological order after checkpointing, unless one indivisible native tool chain alone would leave no room for a valid checkpoint.
- [ ] Protected task state, user constraints, decisions, and open work cannot be silently discarded by deterministic masking.
- [ ] A failed, interrupted, or partial compaction never changes the committed model view.
- [ ] Legacy histories and legacy in-progress split state remain readable without rewriting existing bytes.
- [ ] Provider concurrency failures fall back to bounded sequential mapping without losing successful segment results.
- [ ] Existing behavior is preserved unless it conflicts with the layered context architecture.

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

## Phase 5 — Final invariant and quality validation (current)

- [ ] Replay long immutable traces comparing the old greedy contingency, masking-only, one-shot checkpointing, sequential MapReduce, and parallel MapReduce.
- [ ] Measure task-state retention, critical constraint/decision recall, stale-fact removal, open-work survival, token reduction, request count, latency, and recovery behavior.
- [ ] Cover adversarial traces: one huge item, repeated compactions, changed files after reads, repeated failures, conflicting summaries, and compaction across revert/fork/rollback.
- [ ] Prove every supported compaction operation leaves all previous JSONL bytes as an exact prefix and creates no second authoritative state store.
- [ ] Re-scan persistence and context code for truncation, overwrite, unreferenced summaries, destructive rollback, or view state that cannot be rebuilt from deltas.
- [ ] Run formatting, targeted Clippy/build checks, full workspace tests, and feature-gated checks where practical.
- [ ] Record validation results and any unrelated pre-existing failures here.
- [ ] Mark every invariant complete only after the final audit passes.
