# Layered context compaction

The session JSONL is the only authoritative session history. Context items,
masking decisions, checkpoint metadata, and resumable MapReduce progress are
reconstructed by replaying deltas. A checkpoint is an appended derived summary,
not a replacement transcript. Forks share the parent JSONL; revert and rollback
append references and never modify previously written bytes.

## Model-view policy

1. Deterministically mask old, reacted-to tool results, retaining their raw
   payloads and source IDs in history. User instructions and the live TODO
   block are not masking targets.
2. Reserve a recent raw window (20% of the effective context budget), preserving
   native tool-call/result pairs. Automatic checkpointing covers the older
   prefix; the checkpoint is composed before the raw tail even though its
   event was appended afterward.
3. Build a structured handoff containing the objective, constraints, decisions
   and evidence, invalidations, completed/active/blocked work, next action,
   files, and code anchors. Changed-file observations must be marked stale.
4. When one-shot compaction cannot fit, build independent map summaries and
   hierarchically reduce them. There is no greedy continuation buffer for new
   compactions. Previously persisted legacy split staging remains resumable.

Manual compaction preserves its existing whole-responded-history behavior.
Unanswered or queued user messages remain verbatim when prior responded work
exists. An oversized indivisible source item/tool chain may require folding the
whole eligible source into a checkpoint. A handoff that still exceeds the
commit budget is rejected without changing the committed view.

## MapReduce and recovery

### Summarization model selection

Settings contains a **Summarization models** block, independent of Model Router.
Use **Add summarization model** to open the searchable provider/model overlay;
selecting a model closes it without changing the agent's model or reasoning.
Enter on a numbered entry replaces it. Delete removes it; Alt+Up/Down reorders
it. The inline up/down/remove controls provide the same actions with the mouse.
Long lists scroll to keep the selected row visible.

The ordered list is stored in `setup.json` as
`routing.summarization_models`, using `{ "provider": "...", "model": "..." }`
entries. Its default is `[]`:

- Empty list: use the active agent connector, including its reasoning choices.
- Nonempty list: try only those provider/model pairs, in order. Existing bounded
  request retries run before advancing to the next configured model. Neither
  the agent model nor the built-in auto chain is appended implicitly.
- Cancellation stops the chain. Exhaustion reports failure and preserves the
  source and accepted staging; it does not substitute an unselected model.
- The pseudo-model `auto` is not offered in this list. During an agent loop,
  empty-list auto mode inherits the concrete connector currently selected by
  the agent's auto router. Between turns, `/compact` resolves the configured
  auto chain because no live agent connector exists to inherit.

The setting applies to one-shot summaries, all MapReduce stages (including
audits/corrections/rereads), legacy split resumption, and internal subagent
compaction. Changes apply to subsequently created harnesses, not an already
running turn. Explicit summary models use provider-default reasoning; they do
not inherit the main agent's effort setting. Configured local URLs and the
session's applicable cache preferences are retained. Notifications identify
each attempted summarizer, and usage is attributed to that provider/model.

The summarizer has its own connector and known window. A summarizer overflow
does not resize the agent's context budget or change its connector/fallback
chain. Discovered/reported windows bound summary requests; when discovery is
unavailable the existing context-budget estimate is used until the provider
reports a limit. Checkpoint commit still respects the agent's budget. This
routing change does not resolve the finish-reason, provider-reasoning reservation,
or semantic-recall findings tracked separately in `ISSUES.md`.

### Whole-item segments

Each map has a stable ordinal, exact item ranges, and ordered whole-item IDs.
The packing target is soft: if the next item exceeds the remaining space, move
it intact to the next group and reset the group's accounting. An item larger
than the target occupies its own group; it is never cut at byte or token offsets.
The complete request, including instructions and reserved output, is checked
against the actual model window before sending. An indivisible item that cannot
fit that request fails explicitly with source and accepted progress intact.
It does not trigger textual slicing or unchanged singleton retries.

The application, not the model, owns coverage metadata.
Accepted results survive interruption and restore. Source fingerprints prevent
staging from being reused after a same-ID source replacement. Version-one
byte-sliced MapReduce staging is read for compatibility but restarted from the
immutable source items; fragment-derived summaries are not reused. Saving the
new staging appends deltas, leaving the legacy events and original source intact.
Already committed checkpoints are not rewritten by this staging migration.

Up to four map calls run concurrently. A provider failure stops new scheduling,
drains successful in-flight calls, and persists sequential fallback for missing
maps. Context overflow permits at most three whole-item repartitions of
incomplete maps; accepted summaries are not regenerated. Reducing the packing
target does not reduce the actual request window unless the provider reports
a smaller window. Repartitioning stops when no further item boundaries exist.

Reducers process all ordered summaries, recursively grouping them when needed.
`REREAD: start-end,...` can request up to three raw ranges, including ranges
referenced by an older checkpoint. Malformed, unavailable, or oversized rereads
fail explicitly; partial excerpts never claim complete range coverage. One
reread attempt is permitted per reduction request.

A validator audits the final candidate against the original map summaries.
Corrections are revalidated. Eight reduction levels and two corrections bound
non-converging output. An explicit model switch renews exhausted recovery
budgets while preserving accepted maps; retrying the same model does not.
Generic errors and cancellation are not classified as stuck context windows.

No-tools requests reserve prompt overhead and bound output tokens to 10% of
the active window, clamped to 64–2,000 tokens. These are local estimates, not
provider guarantees; provider window errors remain authoritative. Stream
resets discard partial text, and waits/streams respond to cancellation.

## Reproducible offline evaluation

Run:

```sh
cargo test --lib --no-default-features offline_long_trace -- --nocapture
```

The fixture contains 24 file-inspection turns, a later file invalidation,
constraints, a decision, verified work, open work, and pending user steering.
It compares identical immutable source items through five paths, using
**scripted replies**, not a live model. The 2026-09-05 run produced:

| Path | Estimated view tokens, before → after | Scripted calls | Observed property |
| --- | --- | --- | --- |
| Masking only | 29,337 → 5,757 | 0 | Keeps task facts and raw history |
| One-shot oracle | 29,337 → 1,300 | 1 | Input is 29,635 tokens: infeasible for the 8,000-token fixture window |
| Legacy concatenation oracle | 29,337 → 99 | 8 | Whole-view behavior; no global reducer or audit |
| Sequential MapReduce | 29,337 → 1,300 | 10 | Eight maps, reduction, audit; pending steering stays raw |
| Parallel MapReduce | 29,337 → 1,300 | 10 | Same final view and coverage despite different scheduling |

Both MapReduce modes retained all five scripted task-state markers, excluded the
stale-content marker from the final view, and preserved every original item.
This proves transport behavior for the supplied replies, **not model recall**.
The legacy oracle deliberately supplies the complete answer in its first reply;
its smaller output is not a quality result. After the whole-item correction,
map prompts peaked at 4,017 estimated input tokens. Local mock orchestration
took approximately 9.4 ms sequentially and 7.6 ms in parallel in this run;
these are neither provider latency nor a
reliable speedup benchmark.

Separate tests exercise oversized items, repeated checkpoints, correction
failure, raw conflict rereads, source replacement, model switches, out-of-order
completion, cancellation, partial failures, and resume. The session-store
fixture checks exact JSONL byte prefixes after each persisted stage and after
fork/revert/rollback, including replay equality and absence of a child JSONL.

## Limits and follow-up evaluation

No paid provider experiment or live-model quality comparison was performed.
Structured Markdown and model-based audits cannot guarantee factual recall:
an omission already present in a map can escape an audit against that map.
Oversized raw conflict ranges currently require failure/replanning rather than
an automatic byte-range reread protocol. Accepted staging can grow the JSONL;
it remains derived data in the same history, not a separate session store.

A live evaluation should pin models, provider settings, context windows, and
trace revisions; run each strategy repeatedly on the same traces; score exact
constraint/decision recall, open-work survival, stale-fact rejection, evidence
provenance, and downstream task completion against human-checked source facts;
and report provider tokens, cost, requests, latency distribution, and injected
failure recovery. Keep quality scores separate from deterministic invariant
tests and do not treat a model's own `PASS` as ground truth.
