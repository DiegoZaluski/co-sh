# Fixing Improper Agent-Loop Stops in Cosh

**Status:** implemented and tested (`cargo check` clean; `cargo test -p cosh --lib harness::` → **405 passed, 0 failed**; the 6 new gate tests all pass).

## 1. The problem

The agent loop sometimes stopped silently mid-task. Three distinct stop shapes were confirmed, none of them involving the model calling `stop_agent_loop` (which always fires a toast when used — the bug never did):

1. **Reasoning-only streams.** The model streams its thinking block (Anthropic `thinking` deltas, OpenAI `reasoning` deltas), the stream ends with `finish_reason = "stop"` and **zero answer tokens**. The loop treats it as a finished turn.
2. **Bare empty responses.** `stop` with no tokens and no reasoning at all — still treated as a finished turn.
3. **Intermediate narration stops after tool cycles.** The post-dispatch steering text asked the model for "your response"; the model answered with a status sentence ("Now I'll run the tests.") and stopped. **This exact failure reproduced live, repeatedly, during the development session**: the harness injected the sentence below as a user turn (with the open TODO block visible) and each response stopped without completing the work:
   > "Please continue with your response based on the information above."

Root cause: the natural-stop gate accepted any whitelisted `finish_reason` (`stop` | `end_turn` | `tool_calls`) as *task completion*. But a `finish_reason` describes the **response**, not the **task**. The Anthropic Messages API documents `stop_reason: "end_turn"` as the model having finished *its reply*, not the user's goal; the OpenAI Chat Completions API documents `finish_reason: "stop"` as "the model reached a natural stopping point *of the response*" — both are per-response signals. (References: [Anthropic Messages API — response stop reason](https://docs.anthropic.com/en/api/messages-response), [OpenAI Chat Completions — finish_reason](https://platform.openai.com/docs/api-reference/chat/object).)

The same lesson exists in the OSS ecosystem: opencode PR [#14973](https://github.com/anomalyco/opencode/pull/14973) ("prevent agent loop from stopping after tool calls with OpenAI-compatible providers") had to stop trusting `finish_reason` and inspect the actual message parts for tool calls, because providers (Gemini via LiteLLM) report `"stop"` even when tools were called. Codex issue [#5264](https://github.com/openai/codex/issues/5264) ("Tendency to stop the agent on output while it has a plan to move on") is the same user-visible bug this document fixes; OpenAI fixed it from both ends — harness (`/goal` continuation audits) and model prompts (PR [#7923](https://github.com/openai/codex/pull/7923), "prevent GPT-5.1/5.2 from ending turns after tool preambles").

## 2. The five fixes

### Fix 1 — Reasoning-only streams are retried, with one-turn reasoning promotion

**Where:** `src/harness/core.rs`

- **:5432** — `if self.stop_nudges_left > 0 {` opens the deterministic veto layer, run *before* the optional checkup, so it works in builds with no local decision model.
- **:5439–5441** — the reasoning-only detection: `assistant_response.is_empty() && !self.attempt_reasoning.is_empty()`. `attempt_reasoning` still holds the thinking because it is only drained when a non-empty response is recorded (`add_assistant_with_reasoning` call is inside `if !assistant_response.is_empty()`, previously `core.rs:4495`).
- **:5444–5455** — the reasoning is `take()`-n from the accumulator and composed into the steering message:

  ```text
  [Harness note: your previous response ended right after your reasoning,
   before any answer. Reuse the thinking below instead of reasoning again
   — continue exactly where you left off.]

  <your-previous-reasoning>
  ...the model's own thinking...
  </your-previous-reasoning>
  ```

  Then `continue` — the loop keeps running.

**Why one turn only:** `current_input` is *per-iteration steering state*, never stored in the context manager (`build_messages` appends it to the request and the duplicate-suppression rule in `src/harness/context/mod.rs:1779` skips a duplicated trailing user turn — pinned by `build_messages_appends_current_input_unless_duplicate` in `src/harness/context/test.rs:54`). On the very next cycle the steering message is replaced or cleared, so the reasoning is visible for exactly **one** request and then vanishes back to being display-only. This is the user's requested design: "promote the reasoning to visible for 1 turn, then back to normal."

**Why reuse instead of re-derive:** reasoning is expensive and the model has already solved the planning step; re-deriving it burns tokens and risks a *second* identical reasoning-only stop (the doom loop opencode PR [#15852](https://github.com/anomalyco/opencode/pull/15852) fought). Feeding the thinking back as reference material makes the next turn direct.

**Why a steering message rather than a synthetic context item:** Anthropic validates the cryptographic signature of replayed `thinking` blocks and rejects unsigned ones with HTTP 400 (see [Anthropic docs — thinking blocks in tool use](https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking#thinking-blocks-and-tool-use)); the harness already replays them verbatim through `pending_thinking_blocks` + `push_tool_history` (`core.rs`, `push_tool_history` — thinking blocks are cleared for harness tools precisely because the signature belongs to the real `tool_use` turn). A synthetic promoted item would break that contract, so the promotion rides the steering text instead — valid for every provider dialect.

**Why not the reasoning of ALL prior turns:** only the last attempt's reasoning is still in the accumulator by construction (cleared at every stream start, `core.rs:3022`, and on mid-stream resets, `handle_mid_stream_reset`), which is exactly the "only the last reasoning" requirement — nothing older can leak.

### Fix 2 — The post-dispatch continuation prompt is removed

**Where:** `src/harness/core.rs:5648–5666` (was `:5556–5557`).

```rust
// was:
current_input =
    "Please continue with your response based on the information above.".to_string();
// is:
current_input.clear();
```

**Why removal and not a better sentence:** after tool dispatch the conversation already ends in `tool_calls → tool results`. The provider-standard signal for "the next assistant turn is the reaction to those results" is exactly that tail shape — no user turn needed:

- **OpenAI Chat Completions**: a conversation ending with an assistant message carrying `tool_calls` followed by role=`tool` messages expects the next assistant turn ([reference](https://platform.openai.com/docs/guides/function-calling#handling-function-responses)).
- **Anthropic Messages API**: the assistant `tool_use` block followed by the user-role `tool_result` block requires a next assistant turn — "you must then send a new request with the `tool_result`" ([reference](https://docs.anthropic.com/en/docs/build-with-claude/tool-use#tool-result-format)).
- **Google Gemini function calling**: `functionCall` parts followed by `functionResponse` parts continue the turn loop the same way ([reference](https://ai.google.dev/gemini-api/docs/function-calling#step-4-validate-and-call-the-function)).

Codex implements precisely this: `while (turnInput.length > 0)` re-sends only the accumulated history — tool outputs become the next input, with **no steering sentence between cycles** ([agent-loop walkthrough](https://devbriefs.com/openai-codex-agent-loop-structure-analysis/), [OpenAI's own post](https://openai.com/index/unrolling-the-codex-agent-loop/)). A synthetic "please continue with your response" user turn does two harms: it *asks for a response* (inviting the status-message stop — the mechanism reproduced live in this session, see §1 case 3), and it fabricates a user turn every cycle on the wire, breaking the exact-prefix property Codex deliberately preserves for prompt caching (each request must be an exact prefix extension of the previous one; [OpenAI prompt caching](https://platform.openai.com/docs/guides/prompt-caching)).

**Safety:** `build_messages` already appends nothing when `current_input` is empty (`src/harness/context/mod.rs:1779`, test `context/test.rs:54`), and the Mistral-constraint `skip_while` only drops *leading* tool results (`context/mod.rs:~1690`) — trailing tool results reach the model, so the request always ends in the valid `tool → assistant` continuation shape.

### Fix 3 — An open TODO plan blocks the natural stop (deterministic, no decision model)

**Where:**

- `src/harness/context/todo_ctxt.rs:169` — new `TodoContext::has_open_tasks()` (true when any task is `Pending`/`InProgress`; reuses the existing `all_terminal` predicate at `:155`, the same one driving the block's removal rules, tested at `todo_ctxt.rs:199–230`).
- `src/harness/context/todo_ctxt.rs:178` — new `open_task_descriptions()` returning the open items in order.
- `src/harness/context/mod.rs:721,728` — passthroughs `plan_has_open_tasks()` / `open_plan_tasks()` on the `ContextManager`.
- `src/harness/core.rs:5476–5494` — in the veto layer: if the plan has open tasks, the stop is vetoed and a nudge is injected naming the open items:

  ```text
  [Harness note: the plan still has 2 open task(s). Continue with the next
   one — do not ask whether to continue.]
  - write the report
  - update the changelog
  ```

**Why:** this is the "intermediate narration stop" case (§1 case 3) that reasoning detection cannot see. The rule mirrors what the loop *already* does for other pending work — `/agents` pane requests (`has_outstanding_requests`, `core.rs` wait-for-outstanding block) and background sub-agents (`background::has_running_tasks`) both hold the turn open. The plan was the one pending-work source the gate ignored. The TODO block is a protected tail block re-mirrored from the tools' authoritative `Plan` state (`sync_todo_context`, `core.rs:3610`, called at loop start and after every `plan_*` dispatch), so the check reads ground truth, not the model's claim.

**Why deterministic:** the checkup is optional by design (a local decision model costs >1 GB of RAM and not every user wants it). This layer needs no model at all — it is a state comparison, the same engineering as opencode's `needsContinuation` (derived from observable message parts, not model intent; `packages/core/src/session/runner/publish-llm-event.ts` in `anomalyco/opencode`).

### Fix 4 — A bare empty response is retried once

**Where:** `src/harness/core.rs:5458–5470` — second arm of the veto layer: empty response with *no* reasoning at all → one retry (`"Your previous response was empty. Continue with your work."`), consuming a nudge.

**Why:** an empty completion is a degenerate response, never a finished task. Previously it fell straight through the gate (`&& !assistant_response.is_empty()` in the checkup condition at old `:5400` skipped the audit entirely, and nothing else vetoed it).

### Fix 5 — A stream cut mid-tool-call is never a finished turn

**Where:**

- `crates/cosh-sdk/src/extract_action/extract.rs:295–306` — new `ExtractAction::has_unfinished_stream()`: true when the stream ended with `pending_tool_call` parked (a validated call awaiting delivery that would be silently dropped) or with `depth > 0` (a `{` opened and never closed — the tool-call JSON is incomplete). `deferred_tail` is deliberately excluded: trailing text after a completed call also occurs on healthy ends, so it cannot distinguish a cut from a clean finish.
- `src/harness/core.rs:722` — new harness field `last_stream_unfinished`; set at the end of every stream from the extractor (`:3124` mock path, `:3279` real path), reset at every stream start (`:3043`), including the mid-stream-reset path (`handle_mid_stream_reset` clears extractor state, which the next `has_unfinished_stream()` read re-evaluates).
- `src/harness/core.rs:5388` — the whitelist becomes:

  ```rust
  let finished = matches!(
      self.last_finish_reason.as_deref(),
      Some("stop" | "STOP" | "end_turn" | "tool_calls")
  ) && !self.last_stream_unfinished;
  ```

  An unfinished stream therefore falls into the **existing** `!finished` truncation arm (`"Your previous response was cut off before completion."`) instead of the natural-stop gate.

**Why:** this is the local twin of opencode PR [#14973](https://github.com/anomalyco/opencode/pull/14973): "before exiting the loop, look at the last assistant message parts … If tool parts exist, the model did call tools, so the loop must continue regardless of what `finish_reason` the provider reported." An HTTP stream can close cleanly at the transport layer while the *content* is cut mid-JSON; the `finish_reason` frame says nothing about that.

### Shared infrastructure — the nudge budget

**Where:** `src/harness/core.rs:734` (`stop_nudges_left: u32`), `:919`/`:6491` (init `3`), `:3994` (reset per agent loop).

Every veto consumes one nudge. The budget is deliberately **scarce and separate from `MAX_ITERATIONS`**: `MAX_ITERATIONS` (100, `core.rs:232`) bounds all continuations; the nudge budget bounds *silent* vetoes so a model that keeps insisting on stopping with open work still terminates — visibly, with a normal `Done` event — after 3 nudges instead of looping. This is the symmetric guard against the opposite failure mode (opencode issue [#11153](https://github.com/anomalyco/opencode/issues/11153): "Session loop doesn't stop when model returns finish_reason=stop" — infinite loop from over-vetoing).

Every veto leaves a `laya.log` trace (`log::info!(target: "laya", ...)`) so "why did it stop?" stays answerable — the observability gap that made this bug invisible.

## 3. Tests — mocks identical to what the providers return

All new tests live in `src/harness/test/agent_loop_test.rs` and drive the same accumulation + event path the real stream uses (`with_mock_reasonings` feeds reasoning deltas through the identical accumulator as Anthropic `thinking` / OpenAI `reasoning` deltas; `with_mock_finish_reason` feeds the *normalized* finish values the SDK produces — see `normalize_finish_reason` in `crates/cosh-sdk/src/connector/output.rs:168`, which maps raw provider frames like OpenAI's `{"choices":[{"finish_reason":"stop"}]}` and Anthropic's `{"delta":{"stop_reason":"end_turn"}}`).

The raw wire shapes the mocks stand in for:

| Provider | Frame the mock represents |
|---|---|
| OpenAI-compatible | `data: {"choices":[{"delta":{},"finish_reason":"stop"}]}` — `finish_reason` on the final chunk ([streaming reference](https://platform.openai.com/docs/api-reference/chat/streaming)) |
| Anthropic | `event: message_delta` → `{"delta":{"stop_reason":"end_turn"},"usage":{...}}` ([streaming reference](https://docs.anthropic.com/en/api/messages-streaming)) |
| Gemini | `candidates[0].finishReason: "STOP"` in the final chunk ([reference](https://ai.google.dev/api/generate-content#FinishReason)) |

`connector/output.rs` itself contains recorded fixtures of these exact frames (e.g. `raw: r#"{"id":"","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hi!"},"finish_reason":null}]}"#` followed by `finish_reason: Some("stop")`) — the harness tests reuse the same normalized values.

The six tests:

| Test (line) | Scenario | Assertion |
|---|---|---|
| `natural_stop_gate_continues_a_reasoning_only_stream_and_promotes_the_reasoning_once` (`:2359`) | reasoning deltas + `stop`, zero answer tokens; then the continuation | reasoning promoted **exactly once** (`promoted_reasonings()`); continuation's text is the loop `Closure` |
| `natural_stop_gate_retries_a_bare_empty_response` (`:2395`) | `stop`, zero tokens, zero reasoning; then the answer | retried, nothing promoted, answer is the `Closure` |
| `natural_stop_gate_continues_when_the_plan_still_has_open_tasks` (`:2425`) | clean text turns + `stop` while a `Pending` plan task exists | the 3 vetoed stops each recorded as ordinary `Assistant` items; the loop only ends after the budget, `Closure` = last stream |
| `natural_stop_gate_bounded_three_nudges_then_visible_done` (`:2488`) | model insists on stopping with an open plan, 4 streams | exactly **one** `Done` — visible termination, no hang |
| `a_stream_cut_mid_tool_call_is_never_a_finished_turn` (`:2532`) | stream 1 ends mid-JSON (`{"name": "test_tool", "arguments": {"x":` ) with `finish_reason: stop`; stream 2 re-emits the complete call | the retry call dispatches (`ToolCall` + `ToolResult` events, same assertions as the canonical `test_agent_loop_with_tool_call` at `:66`); a terminal event follows |
| `a_healthy_single_text_turn_still_ends_naturally` (`:2586`) | regression guard: clean answer, no plan, no cut | exactly one `Done` on the first turn; **no nudge consumed** |

The mid-JSON cut mock mirrors what a truncated OpenAI-compatible stream looks like after `normalize_finish_reason`: the SSE frames stopped before the closing `}` of the tool-call arguments, and the HTTP stream nonetheless terminated without an error frame.

## 4. What was intentionally NOT changed

- **The optional checkup stays optional** (`with_checkup`, `core.rs:990`; default `None`) — a local decision model costs >1 GB of RAM; builds without it are first-class. The new gate is deterministic and runs *before* it, so protection no longer depends on the decision model at all.
- **`iteration < MAX_ITERATIONS` stays untouched** (`core.rs` veto bound) — it is the global retry bound; the nudge budget is a separate, much smaller bound for the specific "model insists on stopping" case.
- **Directed steering prompts stay** — the truncation continuation (`:5417`, "Your previous response was cut off…") and the checkup-veto `continue` (`:5468`-region, bare `"continue"`) carry real instructions; only the content-free post-dispatch sentence was removed.

## 5. Summary of touched files

| File | Change |
|---|---|
| `crates/cosh-sdk/src/extract_action/extract.rs` | `has_unfinished_stream()` (`:295–306`) |
| `src/harness/context/todo_ctxt.rs` | `has_open_tasks()` (`:169`), `open_task_descriptions()` (`:178`) |
| `src/harness/context/mod.rs` | `plan_has_open_tasks()` (`:721`), `open_plan_tasks()` (`:728`) |
| `src/harness/core.rs` | `last_stream_unfinished` (`:722`), `stop_nudges_left` (`:734`), test log (`:740`), stream-end capture (`:3124`, `:3279`, reset `:3043`), per-loop budget reset (`:3994`), whitelist hardened (`:5388`), veto layer (`:5432–5494`), prompt removal (`:5648–5666`) |
| `src/harness/test/agent_loop_test.rs` | six new tests (`:2359–2608`) |

Result: every previously-silent improper stop now either (a) cannot happen — the cut shapes are classified as truncation before the gate — or (b) is vetoed deterministically up to 3 times with the model's own reasoning as fuel, and only then terminates, visibly and logged.
