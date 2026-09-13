# Layered context compaction

The session JSONL is the only authoritative session history. Context items,
masking decisions, checkpoint metadata, and resumable MapReduce progress are
reconstructed by replaying deltas. A checkpoint is an appended derived summary,
not a replacement transcript. Forks share the parent JSONL; revert and rollback
append references and never modify previously written bytes.

The protected tool plan is also structured session state. Plan changes append
`ContextDelta::Todo` events; resuming the current head restores both the protected
block and the tool's editable list before the first agent iteration. Empty and
completed plans remain persisted even when their block is not displayed. Older
histories without a recorded plan still load; a missing plan is not guessed from
summary text. Each plan delta binds to its recorded context-item boundary.
Historical fork/revert selections resolve the applicable plan event; rollback
restores the exact prior head, including an empty or completed plan. Branches
carry only derived event references, not duplicate plan histories. An older
plan without a source binding is usable at its head but is not guessed for an
earlier message selection.

## Model-view policy

1. Mask tool results ONLY on the model's explicit `mask_tool_result` request
   (tail masking): the newest tool result is replaced by a typed reference,
   retaining its raw payload and source ID in history. Masking never runs
   deterministically and never touches mid-history positions — a mutation at
   position *k* invalidates the exact-prefix prompt cache of every provider
   for everything after *k*, while a tail mutation is cache-neutral. User
   instructions and the live TODO block are not masking targets. The tool
   lives only in the tool schema (part of the cached `tools → system` prefix);
   forgetting to call it is harmless — the LLM compaction below stays the
   structural fallback.
2. Reserve a recent raw window (20% of the effective context budget), preserving
   native tool-call/result pairs. Automatic checkpointing covers the older
   prefix; the checkpoint is composed before the raw tail even though its
   event was appended afterward.
3. Build a structured handoff containing the objective, constraints, decisions
   and evidence, invalidations, completed/active/blocked work, next action,
   files, and code anchors. Changed-file observations must be marked stale.
4. When one-shot compaction cannot fit, build independent map summaries and
   hierarchically reduce them. There is no greedy continuation buffer for new
   compactions. Legacy staging without completion verification is rebuilt from
   the immutable source, without reusing its generated text.

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

### Completion integrity

New summarization streams must report natural completion (`stop`, Gemini's
`STOP`, or Claude's `end_turn`). Token limits, refusals, paused turns, unknown
reasons, and missing termination signals reject the response before it becomes
checkpoint evidence. A received usage report is still emitted for that rejected
response. Existing bounded retries and explicitly configured fallback models
apply; no extra model is selected. On the next compaction attempt, staging
created before these checks is invalidated with an appended projection update
and a toast, then reconstructed from source through the authorized model chain.

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
staging from being reused after a same-ID source replacement. MapReduce versions
one (byte-sliced) and two (without completion checks), and unversioned legacy
split buffers, are read for compatibility but rebuilt from immutable source.
Version three MapReduce staging retains normally completed responses. Saving the
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

Normal one-shot summarization preserves the selected connector's output
configuration, including provider defaults and any explicit caller setting.
It never borrows MapReduce's segment ceiling or derives a generation limit
from the automatic trigger. Before choosing MapReduce, the selected
summarizer's actual request is checked against its own known window, with
prompt overhead and the output ceiling its adapter actually sends. Unknown
provider output defaults remain unknown; provider errors are authoritative.
MapReduce maps, intermediate reductions and audits retain their separate 10%
window reservation, clamped to 64–2,000 tokens. Final reductions and
corrections preserve the selected connector's output configuration.
Stream resets and one-shot retries discard partial text from both the
candidate and its visible output; waits/streams respond to cancellation.

Source coverage, the recent raw tail, and the checkpoint commit budget belong
to the receiving agent. A smaller summarizer window sizes its requests; it
does not shrink the agent's raw tail or reject an otherwise fitting agent
view. All commit paths measure the annotated text they actually install.
Legacy split commits also require complete source consumption and retain
staging when the candidate is rejected.

An automatic compaction failure defers further threshold-triggered attempts
for the current agent turn, after the existing retries and configured model
chain have run. The agent can continue with the preserved source. A new user
turn or `/compact` gets a fresh attempt; recovery from an actual provider
context-window error remains available. Successful compaction can run again
within the same turn if subsequent work fills the budget.

### Regression: streamed summary, error, one tool call, repeat

The GLM-5.3-flash report (same model for agent and summarizer, 1M context
window, approximately 164k automatic trigger) is consistent with two coupled
bugs: a 2,000-token output ceiling on one-shot summaries when window discovery
succeeded, and a fresh retry budget after every tool dispatch. Input capacity
does not prevent output truncation. A `length` finish reason correctly rejects
the streamed text as incomplete, leaving the original over-trigger context
in place. The next dispatch then used to start the same failing process again.
A fresh manual harness without discovery did not set that output ceiling,
which explains why `/compact` could succeed on the same history.

`summary_failure_loop` reproduces this through a local HTTP/SSE provider and
the real SDK completion checks. Before the fix, two small tool calls produced
three failed compactions, nine requests with `max_tokens: 2000`, and three
`LLM compaction failed: summarizer response is incomplete (finish reason: length)`
toasts. The same source succeeded manually with no explicit output limit.
After the fix, automatic and manual paths each complete with one request and
unchanged connector output settings. Separate tests cover exhausted retries at turn
entry and mid-turn, rejected oversized summaries, manual retry, and a fresh
user turn. The existing successful-regrowth test still permits multiple
successful compactions when new content actually fills the budget.

The persisted local session also corroborates this mechanism: the three
compaction boxes between 02:14 and 02:18 UTC on 2026-09-13 each contain three
`## Objective` headings, omit `## Code & Anchors`, and end mid-sentence/code.
The following box at 02:19 contains the complete section structure and is
followed by the only committed checkpoint. Previously the UI concatenated
the three failed responses inside each box, making the displayed output look
like a much longer summary. Retries now reset that display as well.

These artifacts identify the most likely cause of the incident, but do not
store its original error toast or provider finish reason. Other incomplete
termination signals remain errors; the fix does not accept truncated text as
checkpoint evidence.

See [the boundary audit](context-compaction-audit.md) for the responsibility
boundaries, regression evidence, and validation details.

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
| Masking only (model-driven, all results masked) | 29,337 → 5,757 | 0 | Keeps task facts and raw history |
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
