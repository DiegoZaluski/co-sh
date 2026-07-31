# Bug Report: Context Window Percentage Pinned at 100% and Per-Iteration Harness Bottlenecks

**Date:** 2026-07-31

## Scope

Audited the full path that feeds the TUI budget bar and the per-iteration
costs of `run_agent_loop`:

- `src/harness/context_manager/manager.rs` — `display_info()`, `run()`,
  `compress_oldest`, `compress_if_needed`, `compress_via_llm`,
  `compress_fresh_up_to`, `format_context`
- `src/harness/context_manager/compression/` — `init.rs` (LSA/TF-IDF/MMR),
  `lsa.rs` (full SVD), `mmr.rs` (O(n²) greedy selection)
- `src/harness/core.rs` — `run_agent_loop`, `build_conversation_messages`,
  `build_chat_context`, `push_tool_history`, `dispatch_next`
- `src/harness/tools.rs` — `CoshTools::dispatch` (bash/fs/find paths)
- `src/tui/app.rs` — `render_budget_bar` consumption of `budget_pct`
- `crates/cosh-sdk/src/connector/openai_compatible/caller.rs` — SSE stream
  handling (30s per-chunk timeout)

## Methodology

Each bug was reproduced deterministically by a reproducer test whose assertions
encode the expected (correct) behavior and fail under the current
implementation:

- `src/harness/context_manager/test.rs`
  - `run_does_not_lower_budget_pct_when_buffer_over_budget` — FAILS
  - `run_never_drains_fresh_buffer` — FAILS
  - `run_does_not_drop_queue_entry_when_compression_fails` — FAILS
- `src/harness/test/bug_hunt.rs`
  - `bug08_over_budget_system_message_ships_full_raw_buffer` — FAILS

Suite results: 138 passed; 4 failed (exactly the 4 reproducers). 0 clippy
warnings.

---

## BUG-C1: Budget percentage is pinned at 100% and never recovers

### What happens

`ContextManager::display_info()` computes:

```rust
let fresh_tokens: usize = self.buffer_chunks.iter()
    .map(|fc| estimate_tokens(fc.role.text())).sum();
let compressed_tokens: usize = self.queue.iter().map(|e| e.tokens).sum();
let seed_tokens: usize = self.fixed_contexts.iter().map(|e| e.tokens).sum();
let total = fresh_tokens + compressed_tokens + seed_tokens;
budget_pct = (total / MAX_CONTEXT_TOKENS * 100).min(100)
```

`MAX_CONTEXT_TOKENS` is 10_000. `buffer_chunks` accumulate the raw text of
every user turn, assistant turn, and tool call + result (`push_tool_history`
and `run_agent_loop` call `add_buffer_context` on every turn). The buffer is
only drained by `compress_fresh_up_to`, which is invoked exclusively by the
model-facing `force_compress` tool. The per-iteration tick `run()` only rotates
the circular `queue` via `compress_oldest(None)`; it never touches
`buffer_chunks`.

### Why it happens

Once the raw buffer exceeds 10_000 estimated tokens — a few tool results
already reach this — `total_tokens >= MAX_CONTEXT_TOKENS` holds permanently,
`budget_pct` saturates at 100, and nothing in the agent loop lowers it. The
buffer is unbounded: every dispatched tool call appends its full result to it.

### Reproductor

`run_does_not_lower_budget_pct_when_buffer_over_budget` and
`run_never_drains_fresh_buffer` in `src/harness/context_manager/test.rs`
(4 chunks × 4k tokens; `run().await` leaves `budget_pct` at 100 and all 4
chunks in place).

## BUG-C2: `run()` compresses the queue but never the over-budget buffer

### What happens

`run()` is documented as the per-iteration context tick and is called from
`run_agent_loop` after each dispatch phase. Its guard
`estimate_tokens(&context) >= MAX_CONTEXT_TOKENS` uses `format_context()`,
which includes the full raw buffer — so the guard fires on almost every
iteration — but the action (`compress_oldest(None)`) pops from the `queue`,
which stays empty unless the model invoked `force_compress`. The dominant
cost (the fresh buffer) is therefore never compressed.

### Why it happens

The budget check measures the buffer, but the compression action targets the
queue. The two are disconnected, so the check is a no-op in the common path.

### Reproductor

`run_never_drains_fresh_buffer` (buffer chunks unchanged after `run()`),
`run_does_not_lower_budget_pct_when_buffer_over_budget` (`budget_pct` stays
100).

## BUG-C3: Queue entry silently dropped when compression fails

### What happens

`compress_oldest(None)` pops the oldest queue entry, then calls
`compress_if_needed(&entry)`. When that returns `None` (LLM call failure or
budget not met after `max_retries`), the function logs an error and returns —
the popped entry is never re-queued and never moved to `fixed_contexts`. The
context entry is lost without any user-visible signal.

### Why it happens

The `pop_front` happens before the compression attempt; the failure path does
not restore the entry.

### Reproductor

`run_does_not_drop_queue_entry_when_compression_fails` — enqueues one entry
(tokens=500 > SEED_MAX_TOKENS), over-budget buffer, `run().await`; entry count
(queue + fixed) goes from 1 to 0.

## BUG-C4: Over-budget system message ships the full raw buffer as "compressed" context

### What happens

`build_conversation_messages` — when `total_history_tokens > HISTORY_BUDGET`
(50_000) — pushes a system message built from
`self.context_manager.format_context()`. `format_context()` renders the full
`## Full` section, i.e. every raw buffer chunk verbatim. The model therefore
receives the entire raw conversation on every iteration; nothing is actually
compressed in the message body.

### Why it happens

The over-budget branch reuses `format_context()` (full render) instead of the
compressed/exhibition-only view (`format_compressed_context`). Because the
buffer is never drained (BUG-C2), the payload grows unboundedly per iteration.

### Reproductor

`bug08_over_budget_system_message_ships_full_raw_buffer` in
`src/harness/test/bug_hunt.rs` — 55k-token history; the generated system
message still contains the raw marker text.

## BUG-C5: `compress_via_llm` runs a blocking, non-streaming LLM call inside the agent loop

### What happens

`compress_if_needed` calls `compress_via_llm`, which performs
`self.connector.chat_with_system(...)` — a full synchronous, non-streaming LLM
round-trip, retried up to `max_retries` (3). This runs inside `run()`, which
`run_agent_loop` awaits between iterations. During this time no tokens, tool
events, or progress events are emitted; the TUI shows only the spinner.
`stop_signal` is not polled inside the compression path.

### Why it happens

Compression is driven through the same provider connector as the main
conversation, with no streaming and no cancellation plumbing.

## BUG-C6: Deterministic compression cost grows super-linearly (LSA full SVD + MMR)

### What happens

`deterministic_compression` (invoked by `compress_fresh_up_to` /
`force_compress` and by `compress_if_needed` on near-budget results) runs
`compute_lsa`, which computes a **full** SVD (`SVD::new(matrix, true, true)`)
of the TF-IDF matrix, plus `mmr_select`, whose greedy loop is O(target × n)
with a per-pair cosine similarity. On a large buffer both steps scale
super-linearly with the number of chunks and vocabulary, so a single
`force_compress` on accumulated tool output can block the loop for a long
time with no output.

### Why it happens

Full-rank SVD and the pairwise MMR loop are used without a cap on the input
size; `text_splitter::TextSplitter::new(200)` chunks the entire combined
buffer before the matrix is built.

---

## Compounding per-iteration costs

- `display_info()` is called ~4× per agent-loop iteration (start, after each
  `run()`, twice at exit) and is O(buffer): it runs `estimate_tokens` over
  every raw buffer chunk. With a large accumulated buffer this adds repeated
  CPU work on top of the payload growth.
- `format_context()` caches its render, but every `add_buffer_context`
  invalidates the cache, so the `run()` tick rebuilds the full string (all
  raw buffer chunks + exhibitions) once per iteration.
- `process_sse_response` wraps each `response.chunk()` in a 30s timeout. A
  provider pausing longer than 30s between chunks (e.g. reasoning models
  before the first token) terminates the stream with
  `Network("stream timed out after 30s")` → the harness error path breaks or
  retries the loop, which is perceived as "stall then resumes".

## Impact

- The TUI context percentage is not actionable: it saturates at 100% in any
  session that dispatches a few tools and never reflects recovery.
- The over-budget system message (BUG-C4) grows every iteration, increasing
  per-request payload and latency even when the session is not long.
- Any compression path that fails loses context silently (BUG-C3), and the
  compression paths that do run are blocking, non-streaming LLM calls inside
  the agent loop (BUG-C5/BUG-C6), which matches the reported "spinner for
  minutes with no output" behaviour.

## Verified facts (not hypotheses)

- `MAX_CONTEXT_TOKENS = 10_000`; `HISTORY_BUDGET = 50_000`.
- `buffer_chunks` is mutated only by `add_buffer_context` (append) and
  `compress_fresh_up_to` (drain). `run()` does not drain it.
- `run_agent_loop` calls `context_manager.run().await` after every dispatch
  phase and `display_info()` at loop start, after each `run()`, and twice at
  exit.
- Over-budget branch of `build_conversation_messages` uses `format_context()`
  (full render), not `format_compressed_context()`.
- `compress_oldest` pops before compressing and returns early on failure
  without restoring the entry.
- `process_sse_response` wraps each `response.chunk()` in a 30s timeout;
  a provider pausing longer than 30s between chunks terminates the stream
  with `Network("stream timed out after 30s")`.
