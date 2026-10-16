# The `src/harness/context` module — Cosh's conversation context manager

> **Scope of this document.** This document describes, in increasing depth, the
> `src/harness/context` module of Cosh: the `ContextManager`, its conversation
> items, the visibility model, the protected blocks, the three compaction
> contingencies (one-shot, hierarchical MapReduce, and the legacy *split*), and
> the provider-error window catalog. The **TUI persistence layer** (the
> `session_store`, JSONL deltas, and session reconstruction on the UI side) is
> **out of scope**: it is mentioned only where the module's transport contract
> requires it, without describing how it works internally.

---

## Table of contents

1. [Introduction](#1-introduction)
2. [Intuitive overview: how the module works](#2-intuitive-overview-how-the-module-works)
3. [Terminology and glossary](#3-terminology-and-glossary)
4. [File map](#4-file-map)
5. [`ContextItem`: the atoms of the conversation](#5-contextitem-the-atoms-of-the-conversation)
6. [`ContextManager`: internal fields and why each exists](#6-contextmanager-internal-fields-and-why-each-exists)
7. [Ingestion: how items enter the timeline](#7-ingestion-how-items-enter-the-timeline)
8. [The visibility model](#8-the-visibility-model)
9. [Automatic sweeps](#9-automatic-sweeps)
10. [Cycle termination: `close_loop`, `Closure` and request threads](#10-cycle-termination-close_loop-closure-and-request-threads)
11. [Token counting and budget](#11-token-counting-and-budget)
12. [`build_messages`: from timeline to provider messages](#12-build_messages-from-timeline-to-provider-messages)
13. [Tail blocks: the protected TODO and the tool-correction memory](#13-tail-blocks-the-protected-todo-and-the-tool-correction-memory)
14. [The one-shot LLM compaction](#14-the-one-shot-llm-compaction)
15. [The hierarchical MapReduce contingency](#15-the-hierarchical-mapreduce-contingency)
16. [The legacy contingency: split-and-concatenate](#16-the-legacy-contingency-split-and-concatenate)
17. [Stuck overflow, window discovery and the error catalog](#17-stuck-overflow-window-discovery-and-the-error-catalog)
18. [`ContextManagerState`: the transport projection](#18-contextmanagerstate-the-transport-projection)
19. [Mathematical precision: removing the simplifications](#19-mathematical-precision-removing-the-simplifications)
20. [Tests: what the module guarantees](#20-tests-what-the-module-guarantees)
21. [References and further reading](#21-references-and-further-reading)

---

## 1. Introduction

Every LLM-based coding agent faces the same physical constraint: the model can
only read what fits inside its **context window**, and a long software
engineering session — dozens of file reads, command executions, edits and
reasoning steps — grows far faster than any window. The `src/harness/context`
module is Cosh's answer to that problem.

It is the **single owner** of the conversation: every event that must reach
the model — the user's request, each assistant response, each tool call and
each tool result — enters as an **item** of a **timeline** in display order.
No other component of the *harness* keeps a second "official" copy of the
conversation; the `ContextManager` is the single source of truth at runtime.
From that timeline it produces, on demand, the list of messages ready for the
provider (`build_messages`), and it is responsible for keeping that projection
**within a token budget** — hiding, summarizing or compressing old content as
needed.

Three design decisions shape everything that follows:

1. **The timeline is *append-only*.** Nothing is ever deleted. Compaction,
   garbage sweeps and abandoned-input cleanup only **move visibility
   markers** (`hidden`, `visible_from`, checkpoint coverage). The complete raw
   history stays in the vector, and that is why session *revert*/*fork* can
   "reveal" back exactly what the model stopped seeing.
2. **The manager is synchronous and LLM-free.** It never calls models. When it
   cannot resolve the budget by itself, it **asks** the *harness*
   (`RunOutcome::NeedsLlmCompaction`) to run a summarizer — a dedicated agent
   with its own system prompt — and receives the finished summary via
   `apply_llm_summary`. The module builds the prompt; the *harness* pays for
   the call.
3. **Mid-history mutations do not exist on the automatic path.** Modern
   providers cache the prompt by **exact prefix**; any change in the middle of
   the conversation would invalidate the whole cache. Therefore the only
   accepted view mutations are: hiding items (safe positions only), **tail-only
   masking** (on the model's explicit request), and checkpoints that are
   **appended** at the end and recompose the projection logically, without
   rewriting old text.

The result is a piece that is small in concept but dense in invariants: an
immutable event log, a cheap mutable projection over it, a budget with a
deterministic trigger, and three recovery strategies in a ladder when the
budget overflows.

### Where it lives and how it is exposed

| Path | Role |
|---|---|
| `src/harness/context/mod.rs` | The `ContextManager`, the items, the visibility model, the message projection and the one-shot compaction |
| `src/harness/context/map_reduce.rs` | The hierarchical MapReduce contingency (persistent, resumable staging) |
| `src/harness/context/split.rs` | The legacy *split-and-concatenate* contingency |
| `src/harness/context/summarize.rs` | Summarizer system prompts, templates and transcript serialization |
| `src/harness/context/todo_ctxt.rs` | The `TodoContext`: the protected plan block (`plan_*`) |
| `src/harness/context/error_catalog.rs` | The local catalog of windows observed from provider errors |
| `src/harness/context/test.rs` | The module's main suite (gate: `cargo test --lib harness::context::`) |

The direct consumer is `src/harness/core.rs` (the `Harness`), which keeps a
`context_manager: ContextManager` field and feeds it at the pace of the *agent
loop*. The export events (`HarnessEvent::ContextItemRecorded`, `ContextInfo`,
`ContextSnapshot`) go out through the same *harness* event channel — the UI
merely consumes them.

---

## 2. Intuitive overview: how the module works

Before the details, the complete mental model in six steps:

1. **Ingestion.** During the *agent loop*, every event becomes a monotonically
   numbered item: `add_user`, `add_assistant(_with_reasoning)`,
   `add_tool_call(_with_thinking)`, `add_tool_result(_with_images)`. Auxiliary
   items (`Error`, `UserCommand`) also enter, but they are **display-only**:
   they never reach the model and cost zero tokens.

2. **Projection.** On every iteration, `build_messages(current_input)` walks
   the timeline **once** and produces the provider's native messages.
   Filtering is by visibility: hidden items or items covered by a checkpoint
   do not exist for the model. Checkpoints are recomposed **before** the
   recent raw history — they are physically at the end of the timeline, but
   logically the *prefix* of the conversation.

3. **Budget.** An incremental token counter (memoized, O(1) per read) sums the
   cost of everything the model would see, plus the tail blocks. On every
   cycle, `run()` does two things: runs the useless-chain sweep and compares
   the total against the **80% trigger** of the budget.

4. **Cheap relief first.** Before any summarization, the model itself can
   request `mask_tool_result`: the most recent irrelevant tool result is
   replaced by a one-line typed reference. This only touches the tail — which
   was not cached yet anyway — and is therefore *cache-neutral*.

5. **Compaction as the last resort.** If the total still reaches 80%, the
   `ContextManager` returns `NeedsLlmCompaction`. The *harness* requests the
   ready-made prompt (`llm_compaction_request`), calls the dedicated
   summarizer, and applies the result with `apply_llm_summary`: a
   `Compaction` is **appended** with the exact ID ranges it covers, and the
   coverage hides the source items. The recent tail (up to ~20% of the
   budget) stays verbatim, so the model continues with maximum fidelity.

6. **The contingency ladder.** If the summary prompt itself does not fit in
   the window, the one-shot compaction is replaced by the **hierarchical
   MapReduce** (segments → level-by-level reduction → validation → correction
   → atomic commit). The sequential **split-and-concatenate** is the legacy
   path, kept only to resume staging written by older versions. If none of
   that fits, the overflow becomes **stuck per model** (`overflow_model`)
   until the user switches models — no paid call is burned per attempt.

Two pieces outside the timeline complete the picture: the **protected TODO
block** (the `plan_*` plan, re-rendered at the tail on every request, immune
to every compaction) and the **tool-correction block** (same, mirrored by the
*harness*). Both count tokens, but are not items: they are never summarized
nor persisted by the timeline.

---

## 3. Terminology and glossary

These terms appear throughout the module's documentation and code; fixing them
now avoids ambiguity later.

| Term | Exact meaning |
|---|---|
| **Timeline** | The `VecDeque<ContextItem>` in display order (oldest first). Append-only: items enter, never leave. |
| **Item** | One entry of the timeline. **One item = one message** in `build_messages` — the mapping is 1:1 by construction, which preserves positions. |
| **Item ID** | Monotonic `u64` (`next_id`), starts at 1. It is the currency of everything (visibility, coverage, rereads, cursors). |
| **Display-only** | Item that exists only for the UI/transcript (`Error`, `UserCommand`). `build_messages` skips it; `tokens()` is 0; summarizers never see it. |
| **Visible** | Item the model would receive on the next request: `!is_hidden(item)`. |
| **Hidden** | Item that stays in the timeline but does not go to the model. Three causes: below the legacy `visible_from` boundary, in the `hidden` set, or inside the newest checkpoint's coverage. |
| **Chain** | A `ToolCall` + `ToolResult` pair linked by `call_id`. Hiding one half drags the other (`hide_item`). |
| **Useless chain** | A chain whose result was marked `useless` (e.g. a search with zero matches, or the confirmation of a `mask_tool_result` call itself). Swept automatically. |
| **Masking** | Replacing an item's *projection* with a typed reference (`[Historical tool result masked …]`). Done **only at the tail**, always on the model's request (`mask_tool_result`), never deterministically. |
| **Checkpoint** | A `Compaction` item: the anchor summary produced by the summarizer, with its `covered_ranges`. |
| **Coverage** | Inclusive ID ranges (`ContextItemRange`) hidden by the newest checkpoint. Derived, never persisted separately. |
| **Recent raw tail** | The final window of the timeline that stays verbatim after compaction — sized in tokens (~20% of the budget). |
| **Trigger** | 80% of the budget. Above it, `run()` requests LLM compaction. In manual mode (`/compact`), the trigger becomes 0. |
| **Summarizer** | The dedicated summary agent, with its own prompts in `summarize.rs`. It never sees the main agent loop's system prompt. |
| **Staging** | In-progress state of a contingency (`SplitState`, `MapReduceState`): persistable, resumable, and never touching the timeline before the commit. |
| **Atomic commit** | A summary application that either installs the full checkpoint (and hides the sources) or changes **nothing** in the model's view. |
| **Exact prefix / prefix cache** | The providers' caching mechanism (Anthropic, OpenAI, Gemini): requests sharing the same byte prefix reuse prior processing. The reason behind several of the module's decisions. |
| **Effective window (ECW)** | The fraction of the advertised window the model can actually reason over (see §19). The module's real budget when discovery succeeds. |

---

## 4. File map

```
src/harness/context/
├── mod.rs             (~2.230 lines) Core: ContextItem, ContextManager,
│                      visibility, tokens, build_messages, one-shot
│                      compaction, ContextManagerState snapshot.
├── map_reduce.rs      (~1.440 lines) Hierarchical contingency: staging,
│                      phases, packing, reread, validation, commit.
├── split.rs           (~360 lines)   Legacy split-and-concatenate contingency.
├── summarize.rs       (~330 lines)   System prompts (one-shot/split/map/
│                                      reduce/validator), templates and
│                                      serialize_item.
├── todo_ctxt.rs       (~460 lines)   TodoContext: the protected plan block.
├── error_catalog.rs   (~200 lines)   The api_errors.json window catalog.
└── test.rs            (~1.700 lines) The main suite (cfg(test)).
```

Organization details that matter:

- `mod.rs` declares the submodules as **private** (`mod map_reduce;` etc.) and
  re-exports the public types selectively (`MapReducePhase`,
  `SplitChunkRequest`, `error_catalog_window`, …). The module's public surface
  is exactly what the *harness* needs.
- `split.rs` and `map_reduce.rs` implement methods **on** `ContextManager`
  (`impl ContextManager` outside `mod.rs`). This is possible because they are
  descendants of the context module: Rust lets submodules access private
  fields of the type declared in the parent. The contingencies are not
  separate types that *wrap* the manager — they are capabilities of it, which
  avoids duplicating state.
- `test.rs` is `#[cfg(test)]` and covers ingestion, rendering, sweeps, budget,
  compaction, snapshot round-trips, `/compact`, window discovery and both
  contingencies — it is the executable specification of nearly everything this
  document claims.
- `todo_ctxt.rs` and `error_catalog.rs` have their own internal suites
  (`mod tests` inside the file).

---

## 5. `ContextItem`: the atoms of the conversation

```rust
pub enum ContextItem {
    User { id, original, synthetic },
    Assistant { id, original, closable, reasoning },
    ToolCall { id, call_id, name, arguments, thought_signature, thinking_blocks },
    ToolResult { id, call_id, content, useless, images },
    Closure { id, content, reasoning },
    Compaction { id, summary, covered_ranges },
    Error { id, content },
    UserCommand { id, content },
}
```

Each variant exists for a precise reason:

### `User`

The user's request. **Protected by construction**: the original text
(`original`) stays verbatim until the LLM compaction folds it into the general
summary — no other phase touches it. The `synthetic` field is **provenance**,
not behavior: `false` = the user typed it; `true` = a turn the *harness*
classified as injected on the user's behalf. Important for accuracy: in
production today, the `[automated notification]` background sub-agent
completions enter through `add_user`, with the marker in the **text**;
`add_synthetic_user`/`synthetic` is the dedicated provenance mechanism,
currently without a production caller. The model-facing rendering is identical
either way; the field exists for the intended consumer — an audit that needs
to filter non-typed turns. Today, however, `turn_user_thread` (the accessor
with that filter) is a **seam without a production caller**: `checkup.rs`
documents explicitly that the current checkup audits only the final text, and
that `turn_user_thread` remains the seam a future *request-aware* checkpoint
will need. The `#[serde(default)]` attribute keeps histories written before
the field existed decodable.

### `Assistant`

An assistant text output. `closable == true` marks a **purely textual** turn
(no tool call) — the only one eligible for promotion to a `Closure` by
`close_loop`. A turn that carried a tool call is structural and never closable.
The `reasoning` field is the model's reasoning **in that response**, and it has
a strict, triply-justified contract:

- **Display-only**: never enters `build_messages` — the model must not see its
  own thinking as assistant prose (and the API would not accept it in that
  position anyway);
- **Zero cost**: `tokens()` never counts it — inflating the budget with text
  the model will never receive again would distort the trigger;
- **Write-once**: set at the item's birth, never mutated in place afterwards.
  Incremental deltas serialize whole items; in-place mutation would re-emit the
  item on every save.

`reasoning: None` is normalized even when an empty string arrives
(`.filter(|r| !r.is_empty())`), and `skip_serializing_if` keeps items without
reasoning **byte-identical** to the format that predated the field — no bloat
in the deltas, compatible with old histories.

### `ToolCall`

The tool call. Structural: **never prose-compressed** — it renders as an
`assistant` message with native `tool_calls`. Two replay fields **exist**
because two provider families **reject** the replay without them:

- `thought_signature`: the signature of the native `functionCall` from
  **Gemini 3.x**, replayed verbatim in the next history (the API rejects the
  call without it). Empty for inline-JSON calls and for every other provider.
- `thinking_blocks`: the **Claude** extended-thinking blocks that preceded the
  call in the original response, replayed verbatim (text + signature) at the
  start of the following assistant message; the API answers 400 to modified or
  missing blocks. Empty for all others.

Both carry `#[serde(default)]` for the same backwards-compatibility reason,
and both **count tokens** (see §11) — they are real request content.

### `ToolResult`

The tool result, linked to its call by `call_id`. Also structural. Two
behavior fields:

- `useless`: marks a result with no useful output (e.g. a zero-match search —
  `find_grep`/`find_glob` signal `"useless": true` in their JSON, and the
  *harness* gates on the tool name in `result_is_useless`; the confirmation of
  a `mask_tool_result` call is useless by definition too). The chain dies on
  the next `run()` — without waiting for a budget overflow — because it served
  its purpose the moment the model reacted. The **newest chain is spared**
  until the model reacts, except when a `mask_tool_result` call itself counts
  as the reaction (§9.3).
- `images`: the multimodal channel — screenshots (`computer_screenshot`) ride
  as base64 `ImageBlock`s next to the text. They are **never** serialized into
  the session history (the base64 would bloat every delta); due to
  `#[serde(skip)]`, they are empty after a **serialization round-trip** (an
  in-memory save/restore preserves them — it is a plain clone of the items):
  they are a transport detail of the current run. When the result is masked,
  the images are replaced along with the text — continuing to send the PNG of
  a masked screenshot would defeat the mask's purpose at full token cost.

### `Closure`

The final response of a completed *agent loop*. `close_loop` replaces the raw
text of the last `Assistant` **in place, with the same ID** — it is a
transform, not a hide: identity and visibility are unchanged, and the token
cache is adjusted by the exact delta. The promotion is verbatim: the final
text is delivered word-for-word for as long as it stays in the timeline (a
future compaction still folds it into the summary, like everything else). The
draft's `reasoning` is carried into the `Closure` — the promotion must not
silently drop the thinking the item was born with.

### `Compaction`

The checkpoint derived by the LLM compaction. Appended at the end of the
timeline (physically), but recomposed **before** the raw tail (logically) — it
is the conversation's prefix. The `covered_ranges` field is the module's
structural novelty: **the exact inclusive ID ranges of the source items** that
this checkpoint folds. Direct consequences:

- the model's coverage is derivable from the item itself — **there is no
  second persisted visibility state** for checkpoints;
- a later checkpoint that folds an earlier one **inherits** its ranges (merge),
  so the newest checkpoint's coverage describes exactly everything folded;
- *revert*/*fork* that removes a checkpoint **automatically reveals** its
  sources, with no compensating visibility mutation.

An empty `covered_ranges` denotes legacy records written before the field.

### `Error` and `UserCommand`

The two display-only items. `Error` records a terminal API/runtime failure for
the user; `UserCommand` records a shell command typed by the user in Command
mode (the TUI as a plain terminal), and is born **directly hidden**
(`add_hidden_user_command` calls `hide_at`). Both: zero tokens, skipped by
`build_messages` and the summarizers, persisted only so the transcript restores
them with their own visual semantics (the styled error box; the command line).
`UserCommand` bears that name — not `Command` — because the agent has its own
notion of shell commands (bash tool calls) and the ambiguity would be lethal.

### `tokens()` and `call_id()`

`ContextItem::tokens(enc)` estimates the cost of what the item **renders
today** with the active model's encoding: `User`/`Assistant` count `original`;
`ToolCall` counts name + arguments + thought_signature + every thinking block
(text and signature); `ToolResult`/`Closure`/`Compaction` count the content;
`Error` and `UserCommand` are **0**. `reasoning` is never counted. `call_id()`
returns `Some` only for tool items — it is the key of the chains.

---

## 6. `ContextManager`: internal fields and why each exists

```rust
pub struct ContextManager {
    binding_tx: Option<UnboundedSender<HarnessEvent>>,
    items: VecDeque<ContextItem>,
    next_id: u64,
    max_tokens: usize,
    encoding: TokenEncoding,
    overflow_model: Option<String>,
    split: Option<SplitState>,
    map_reduce: Option<MapReduceState>,
    visible_from: Option<u64>,
    hidden: HashSet<u64>,
    masked: HashSet<u64>,
    checkpoint_coverage: Vec<ContextItemRange>,
    todo: TodoContext,
    correction_block: Option<String>,
    correction_block_tokens: usize,
    todo_restore_pending: bool,
    summary_annotator: Option<Box<SummaryAnnotator>>,
    cached_items_tokens: usize,
    manual_compaction: bool,
    last_tool_set: Option<Vec<String>>,
}
```

Why each one exists:

- **`binding_tx`** — the optional event channel. When installed
  (`set_binding_sender`), every `push_item` emits
  `HarnessEvent::ContextItemRecorded` (with the `user`, `call_item_id`,
  `tool_name`, `compaction` flags; errors do not emit — the consumer binds the
  error snapshot *after* creating the message). It is the point where the
  timeline talks to the world without coupling to it.
- **`items` / `next_id`** — the timeline and the identity counter. `next_id`
  starts at 1; IDs are the currency of everything (visibility, coverage,
  rereads, cursors).
- **`max_tokens`** — the budget. Default `MAX_CONTEXT_TOKENS = 100_000`;
  overridden by window discovery (`from_window`, `with_discovered_context`) or
  `set_max_tokens`. The trigger re-scales with it, so compaction fires at a
  sane fraction of the model's **real** window instead of a fixed default far
  below it.
- **`encoding`** — the tiktoken `TokenEncoding` resolved by `set_model`
  (default `Cl100k`). The *harness* calls `set_model` at every loop start; if
  the encoding did not change, the function returns early — recomputing would
  re-pay the O(n) tiktoken pass the cache exists to avoid.
- **`overflow_model`** — the model whose window **overflowed** and whose
  overflow the contingency could not relieve. While set (same model), the
  *harness* skips the doomed summarizer call (it would burn a paid call per
  dispatch) and only re-notifies. The key is **model, not provider**: the stuck
  constraint is the model's window; switching models within the same provider
  must release it (`sync_model`).
- **`split` / `map_reduce`** — the persistent staging of the contingencies.
  MapReduce stores **references and derived summaries, never a second copy of
  the raw transcript**; the legacy split keeps buffer + cursor. Both resume
  exactly where they stopped after a restore.
- **`visible_from`** — the legacy compaction boundary. New checkpoints derive
  coverage from their own ranges, but the field remains so histories written
  by old versions can be replayed **without rewriting them**.
- **`hidden`** — IDs hidden one by one (the useless-chain sweep, abandoned
  inputs, cancelled-run debris, user commands). IDs below `visible_from` are
  implicitly hidden by the first clause of `is_hidden`; the code comments
  mention an eventual pruning of those IDs during compaction, but **no path in
  the module removes entries from `hidden` today** — the set only grows (and
  `masked` is cleaned at commit and restore).
- **`masked`** — IDs of results whose **projection** is the typed reference.
  Filled **only** by the model's explicit requests; never deterministically,
  because a mid-history mutation would invalidate every provider's prefix
  cache (§14 states the same restriction from the other side).
- **`checkpoint_coverage`** — the derived coverage of the newest
  **base-visible** checkpoint. Rebuilt from the immutable item on restore
  (`rebuild_checkpoint_coverage`); never persisted separately. Intentionally
  derived: removing the anchor via revert/fork reveals the sources by itself.
- **`todo`** — the `TodoContext` (§13.1). Not an item.
- **`correction_block` / `correction_block_tokens`** — the tool-correction
  memory block, mirrored by the *harness* before every request
  (`set_correction_block`; blank text clears it). **Transient**: never
  persisted; a restored manager starts empty. The token count is memoized so
  `total_tokens()` stays O(1). Why the tail: §13.2.
- **`todo_restore_pending`** — the transient rehydration signal, never
  authoritative state nor persisted: `restore_state` sets it, and the consumer
  drains it via `take_restored_todo_list` (including an explicit reset for
  absent plans).
- **`summary_annotator`** — the *handoff/tool-set consistency* hook: a closure
  installed by the *harness* (`set_summary_annotator`) that rewrites the
  summary **before commit**. The ownership split is deliberate: the *harness*
  knows the effective tool set; the manager owns the single commit funnel
  (`commit_summary_to_source`) — one-shot, MapReduce and the legacy split all
  pass through it. The annotation is `annotate_summary_tool_set` in the
  *harness*: it removes old notices (`[Tool set notice]:`), detects tool names
  cited in the summary that no longer exist in the current set, and appends a
  notice (capped at 8 names) — so the model does not rely on tools that
  disappeared (mode switch, disabled tools, MCP server drift).
- **`cached_items_tokens`** — the incremental token total of the items. There
  is **no per-item map**: a removal re-tokenizes the doomed item (removals are
  rare and bounded), which keeps the cache↔items sync trivial and puts
  *drift* — the failure mode that motivated a debug-only invariant —
  structurally out of reach (§11.3).
- **`manual_compaction`** — the `/compact` mode. While set, `trigger()`
  returns 0 and `run()` hands off to the LLM summary regardless of free
  budget. Scheduling-only, never persisted: the *harness* brackets the whole
  manual pass with `begin_manual_compaction`/`end_manual_compaction`, and a
  restored manager is never left mid-manual.
- **`last_tool_set`** — the effective tool set last offered (sorted,
  deduplicated). Persisted across turns so the *harness* can detect **tool-set
  drift** and notify the model when the set changes.

### Construction and window discovery

- `new(max_tokens)` — explicit budget.
- `from_window(Option<usize>, effective)` — pure, no I/O. A **real**
  discovered window is resized to its **effective** value when `effective` is
  set; the fallback (`None`) stays `MAX_CONTEXT_TOKENS` — the default is not
  any model's window and must not be "shrunk" as if it were.
- `with_discovered_context(model, effective)` — discovers the window via
  public APIs (the models.dev catalog, OpenRouter, Anthropic; catalogs cached
  on disk under `~/.local/share/cosh/cosh/cache`) and delegates to
  `from_window`.
- `resolve_window(window, effective)` — pure: the effective value
  (`effective_context_window` from the SDK, §19.1) or the full window.

---

## 7. Ingestion: how items enter the timeline

All ingestion goes through `push_item`, which does three things **always in
the same order**: emits the recording event (if `binding_tx` exists), adds the
item's cost to the token cache, and pushes onto the `VecDeque`. The order
guarantees the cache never sees an item that is not yet in the timeline, nor
the other way around.

| Method | Creates | Notes |
|---|---|---|
| `add_user(text)` | `User { synthetic: false }` | Protected by construction. |
| `add_synthetic_user(text)` | `User { synthetic: true }` | Identical to the model; differs only in provenance. No production caller today — the `[automated notification]` notifications enter via `add_user`, marker in the text (see §5). |
| `add_assistant(text, closable)` / `add_assistant_with_reasoning(…)` | `Assistant` | Empty `reasoning` is normalized to `None`. |
| `add_tool_call(call_id, name, args)` | `ToolCall` without replay metadata | The empty-signature path. |
| `add_tool_call_with_thinking(…)` | `ToolCall` with `thought_signature` and/or `thinking_blocks` | Native Gemini 3.x / Claude replay. |
| `add_tool_result(call_id, content)` | `ToolResult { useless: false }` | — |
| `add_tool_result_flagged(…, useless)` | idem, with the flag | Explicit useless chain. |
| `add_tool_result_with_images(…, images)` | idem, with images | Multimodal channel; images never serialized. |
| `add_error(text)` | `Error` | Display-only; no recording event emitted (the consumer binds it after the terminal event). |
| `add_hidden_user_command(text) -> u64` | `UserCommand` **born hidden** | Uses `hide_at` (not a bare `hidden.insert`) to also drop the cost from the cache — the drift invariant stays honest. |
| `add_hidden_command_execution(call_id, command, result) -> (u64, u64)` | A `bash_run` `ToolCall`/`ToolResult` pair **born hidden** | The transcript shows what the "terminal" ran; the model never sees it. |

Ingestion accessors:

- `last_user_equals(text)` — true when the last visible, "conversational"
  item (skips `Compaction`/`Error`) is a `User` with that text. The *harness*
  uses it to avoid duplicating the current input when it was already loaded
  via `with_history`.
- `turn_user_thread()` — the **request thread** of the current cycle: starts
  after the last visible `Closure`/`Compaction` (the previous cycle ended
  there; a request from an unrelated task 40 minutes ago must not sit next to
  the current one), filters `synthetic` and hidden items, and returns at most
  `TURN_THREAD_MAX_TURNS = 4` turns: **the first** (the original intent) and
  **the last 3** (the most recent instructions) — the middle is truncated
  first, because the two ends carry the discrimination a *request-aware*
  termination audit would need. Today the method is a **seam without a
  production caller**: `checkup.rs` documents that the current checkup audits
  only the final text and leaves the request thread to a future checkpoint
  (the consumer that actually reads the model's last text today is
  `last_assistant_text`, used by the heuristic-guard audit).
- `last_assistant_text()` — the most recent assistant text, scanning backwards
  through `Assistant` **and** `Closure`. Works **before** `close_loop` (it
  scans through tool calls/results); `None` when the model never produced text
  (a heuristic guard fired over pure tool work). Used by the audit of
  heuristic stops (`MAX_TOOL_RETRIES`/`MAX_ITERATIONS`/loop detection).

---

## 8. The visibility model

This is the heart of the module. Every "what does the model see" decision goes
through a single function:

```rust
fn is_hidden(&self, item: &ContextItem) -> bool {
    let id = item.id();
    id < self.visible_from.unwrap_or(0)          // 1. legacy boundary
        || self.hidden.contains(&id)             // 2. individually hidden
        || self.latest_checkpoint_covers(id)     // 3. covered by the newest checkpoint
}
```

1. **`visible_from` (legacy)** — everything below the boundary is hidden.
   Kept only to replay old histories without rewriting them; new checkpoints
   do not advance this boundary — they use coverage.
2. **`hidden`** — individual hiding, always via `hide_at(idx)`, which:
   registers the ID, **removes the ID from `masked`** (a hidden item needs no
   mask), subtracts the item's **visible** cost from the token cache, and — if
   the item was a `Compaction` — rebuilds the coverage and recomputes the
   cache (hiding an anchor changes which sources become visible again, which
   changes the total cost non-locally).
3. **`checkpoint_coverage`** — the ranges of the newest **base-visible**
   checkpoint. "Base-visible" is the subtlety: `rebuild_checkpoint_coverage`
   scans backwards and takes the first `Compaction` whose ID is not below
   `visible_from` nor in `hidden` — a checkpoint that is itself hidden does
   not count, and coverage falls back to the previous one's (or to none).

`hide_item(idx)` is the **chain-aware** version: for tool items, it locates
the partner (the `ToolCall` with the same `call_id` if the item is a result,
or vice versa) and hides **both halves** — an orphan `tool` message or an
orphaned tool call is rejected by providers that validate the native pairing.
With no partner (a legitimate orphan), it hides only the item. When there is a
partner, it hides the larger index first and then the smaller one (the order
matters because `hide_at` operates by position and the removals here do not
shrink the vector — but the discipline avoids future traps).

**Why visibility instead of deletion?** Because the append-only timeline is
what makes *revert* and *fork* trivial: undoing a checkpoint is removing /
restoring one anchor item, and the derived coverage makes the sources reappear
with no compensating mutation. A history that deleted items would have nowhere
to return to.

### Masks: `masked`

Masking is different from hiding: the item remains **visible**, but its
projection is the one-line typed reference:

```
[Historical tool result masked from the active view; source context item #N
remains in the immutable session history.]
```

`mask_newest_tool_result()` (the *harness tool* `mask_tool_result`):

1. collects the `call_id`s of calls named `mask_tool_result` — **the results
   of the mask confirmations themselves are never targets** (a reflex must
   mask a real tool result; the confirmation's chain is swept as useless
   anyway);
2. finds, back to front, the most recent visible result that is neither masked
   nor a mask confirmation;
3. inserts the ID into `masked` and adjusts the cache: subtracts the item's
   **visible** cost (which, if already masked, was already the reference) and
   adds the reference's cost.

`visible_item_tokens(item)` is the accessor that encapsulates this: a masked
result costs the reference, not the content. Idempotent: already masked or
nothing to mask returns `None` (the *harness* answers the model with
`{"masked": false, "error": …}`).

**Tail only.** Deterministic mid-history masking existed and was retired:
invalidating every provider's cached prefix costs more than the relief saves.
The tail is new, uncached content — touching it is free. That is why the mask
is always "the **newest** result" and not "result N".

---

## 9. Automatic sweeps

### 9.1 Abandoned inputs (`remove_abandoned_inputs`)

Scenario: the user cancels a run (Esc) and types something new. The old input —
which produced **nothing** — is dead weight in the request. The *harness*
calls this method at loop start, right after adding the fresh input. It is a
loop to quiescence with two rules:

- **`drop_abandoned_user_runs`**: when the timeline ends in **two or more**
  consecutive visible `User` turns with no output between them (text, tool
  work, Closure, summary), every turn but the newest is an abandoned input —
  hidden. A turn that produced any output after it is never touched. A hidden
  `User` is *skipped*, not a break — a run already hidden by a previous pass
  never trips the loop.
- **`drop_orphan_calls_before_trailing_users`**: a `ToolCall` **without its
  result** wedged between the old turns and the new input is debris from a run
  cancelled mid-execution — and such an orphan would be **rejected** by
  providers that validate the native pairing, which would then hide the
  legitimate following input too. The scan walks the tool-item zone back to
  front until the first non-tool item; **completed** chains (a call WITH its
  result — including mixed parallel runs where only some results arrived) are
  preserved whole, and a result of a preserved call does not end the zone (an
  orphaned parallel call can sit next to a completed one). An orphan at the
  very **end** of the timeline (a result still pending from a live run) is
  never touched.

Everything is append-only: the debris stays in the timeline, hidden —
revert/fork still reaches it.

### 9.2 The useless-chain sweep (`sweep_useless_chains`)

Runs at the start of **every** `run()`, regardless of the budget. Algorithm:

1. `newest_call_id` = the `call_id` of the newest visible chain — the identity
   of the chain the model has **not reacted to yet** (the call is the identity;
   both halves are excluded together). Already-hidden chains are inert:
   re-hiding would be a no-op, but scanning them could anchor
   `newest_call_id` wrongly.
2. `useless_call_ids` = the `call_id`s of visible results marked `useless`.
3. `masked_call_ids` = the `call_id`s of explicitly masked results — the mask
   **is** the model's reaction, so the newest-chain protection does not apply
   to them.
4. A back-to-front scan (hiding never invalidates a pending index;
   `hide_item` hides the partner, which the next iteration skips naturally):
   hides every chain whose result is useless, except the newest unmasked one.
   Idempotent by construction.

### 9.3 `run()` — the per-iteration tick

```rust
pub fn run(&mut self) -> RunOutcome {
    self.sweep_useless_chains();
    if self.total_tokens() < self.trigger() { Resolved } else { NeedsLlmCompaction }
}
```

The code comment is an architecture decision in itself: **there is no
deterministic masking here**. Mid-history mutations invalidate every
provider's exact-prefix cache for everything after the changed position.
Budget relief near the trigger comes from the model's explicit
`mask_tool_result` calls (tail-only, cache-neutral) or from the LLM
compaction — never from a mid-history mutation.

---

## 10. Cycle termination: `close_loop`, `Closure` and request threads

The *harness* agent loop runs in cycles: a user request, a sequence of tool
iterations, and a final text response. `close_loop` marks the end:

- **Guard**: it only promotes if the **last item** is an `Assistant` with
  `closable == true`. If the last item was a tool call, that text was not a
  summary of what was done — no `Closure` is created.
- **Verbatim in-place promotion**: same ID, original content, the `reasoning`
  carried over. The token cache is adjusted by the exact delta
  (`old_tokens` → `new_tokens`), not recomputed.
- **Effect**: `final_answer()` returns the content when the last item is the
  `Closure` — the canonical read of the cycle's result (the *harness* nests
  sub-agents and reads exactly from there). A turn that ended in a tool call
  returns `None`.

The `Closure` is also the **cycle boundary** for `turn_user_thread` (§7): the
next cycle starts after it. And it is the last thing protected before
compaction: while it stays in the timeline, it renders verbatim.

---

## 11. Token counting and budget

### 11.1 The encoding

`TokenEncoding` (`src/util/mod.rs`) routes by **model name** to the closest
tiktoken encoding: `O200k` (GPT-4o/o1/o3/o4/GPT-5), `O200kHarmony` (gpt-oss),
`P50k` (davinci/codex), and `Cl100k` as the universal default/fallback for
non-OpenAI models (Claude, Gemini, Llama…). tiktoken-rs's registry resolves
known OpenAI models; a substring fallback covers modern dateless aliases
(`gpt-4.5`, `o3`…); names are lowercased (some providers deliver `GPT-4o`).
`estimate(text)` is `encode_ordinary(text).len()` with the BPE tables in
`lazy_static` — the expensive vocabulary is loaded once per encoding.

The estimate is **approximate by design**: without per-provider vocabularies,
it is the best possible cross-provider estimate, and the budget is sized with
slack (effective window + the 80% trigger) to tolerate the error.

### 11.2 The incremental cache

`cached_items_tokens` sums the **visible** cost of all items
(`visible_item_tokens` — masked results cost the reference). The sync points,
exactly the eight of the mutating surface:

| Mutation | Adjustment |
|---|---|
| `push_item` | adds the item's cost |
| `hide_at` | subtracts the visible cost |
| `close_loop` | exact delta (old → new) |
| `mask_newest_tool_result` | visible cost → reference cost |
| `apply_summary_to_source_with_trigger` (via the projection) | projected validation; the commit calls `recompute_cached_tokens` |
| `set_model` (encoding changed) | recomputes everything |
| `restore_state` | recomputes everything |
| `set_correction_block` | recomputes the block memo |

There is no per-item map. A removal re-tokenizes the doomed item (removals are
rare and bounded): keeping a map in sync with the items was the failure mode
that motivated the invariant below; without the map, drift becomes
structurally out of reach.

### 11.3 The debug-only invariant

```rust
#[cfg(debug_assertions)] {
    let brute = /* brute-force visible sum of all items */;
    debug_assert_eq!(self.cached_items_tokens, brute, "cached token total drifted…");
}
```

Every `total_tokens()` call — which the whole suite makes constantly —
cross-checks the cache against a brute-force recompute, failing fast on drift.
Compiled out in release: zero cost. The `saturating_*` in all arithmetic is
exactly what would **mask** a silent underflow; the invariant exists because
saturating arithmetic hides errors.

### 11.4 The total

```rust
fn total_tokens(&self) -> usize {
    self.cached_items_tokens
        .saturating_add(self.todo.tokens(self.encoding))
        .saturating_add(self.correction_block_tokens)
}
```

Items + TODO block + correction block. **Hidden items do not count** — the
cache represents what the provider would be sent, never the raw history. That
is what makes the useless-chain sweep real budget relief, not just cosmetics.

### 11.5 The trigger

```
trigger = 80% of max_tokens        (COMPACT_PCT = 80)
trigger = 0                        in manual mode (/compact)
```

`has_compactable_content()` answers "is there anything to compact?": there is
**at least one visible item that is not itself the checkpoint** — with
append-only, the timeline grows forever, and the test is about *visibility*,
never raw length.

`display_info()` returns the cheap status-bar snapshot: `total_tokens`,
`budget_pct` (0–100, saturated) and `max_tokens`.

---

## 12. `build_messages`: from timeline to provider messages

A single O(n) pass over the items; **one item = one message**, positions
preserved by construction. The pipeline has five stages:

### 12.1 The visibility filter

```rust
let visible: Vec<&ContextItem> = self.items.iter().filter(|it| !self.is_hidden(it)).collect();
```

O(1) per item (boundary compare + hash lookup). Pre-compaction and
individually hidden items never reach the model. Orphans "should not exist"
thanks to the chain-aware hiding — but a restored snapshot could still carry
them.

### 12.2 Checkpoint recomposition

```rust
let projected = visible-that-are-Compaction  ⟶  visible-that-are-not
                 .skip_while(is ToolResult);
```

- **Checkpoints first.** They are items appended at the end of the timeline,
  but semantically the conversation's **handoff prefix**. Internal chronology
  is preserved within checkpoints and within raw items.
- **The `skip_while` over leading tool results**: a `tool` message right after
  `system` is rejected by some providers (e.g. Mistral). Orphans at the front
  of the conversation are dropped from the projection.

### 12.3 Per-variant rendering

| Item | Message | Notes |
|---|---|---|
| `User` | `user`, text verbatim | `synthetic` changes nothing here. |
| `Assistant` | `assistant`, text | — |
| `ToolCall` | `assistant` with native `tool_calls` | `thought_signature` only when non-empty; `thinking_blocks` ride on the message when present. |
| `ToolResult` | `tool` with `tool_call_id` | Masked → the typed reference (**text and images** replaced); no images → text; with images → `tool_result_message_with_images` (each *caller* lowers to its provider's native image parts; text-only backends degrade to the text). |
| `Closure` | `assistant`, content verbatim | — |
| `Compaction` | `assistant`, the summary | — |
| `Error`, `UserCommand` | — (skipped) | Display-only by contract. |

### 12.4 The steering input

`current_input` is the *harness*' per-iteration steering message (e.g.
"please continue with the tool results"). It is appended as the final `user`
message — **unless it duplicates** the trailing `user` turn already present
(the *harness* already owns it via `add_user`; `last_user_equals` exists for
the same purpose at ingestion).

### 12.5 The tail blocks

Finally, the two blocks that **are not items** — tool corrections first, the
TODO block second — are appended. If the last message is already `user`, they
are **merged into it** (blocks first, steering after) instead of injecting a
second consecutive `user` message — rejected by some providers. The full
rationale is in §13.

`export_markdown()` is the transposition of the **same projection** into a
Markdown transcript (sections `## User`, `## Assistant`, `### Tool call: name`,
`### Tool result` — with the `### Tool result (no useful output)` variant for
results marked `useless` — and `## Compaction checkpoint`), with the same
filters and the same masking. Two helpers protect the format: `push_fenced`
picks a fence **longer than any backtick run in the body** (payloads with
their own fences — diffs, docs — never break out of the block), and
`push_section` separates adjacent sections. Errors and commands are left out
(the model never saw them). The *harness* steering input and the tail blocks
are per-request scaffolding, not transcript, and are left out.

---

## 13. Tail blocks: the protected TODO and the tool-correction memory

Two blocks live **outside** the timeline and are rendered at the tail of every
request. The common reason: the **exact prefix cache**. Every modern provider
(Anthropic `cache_control` auto mode, OpenAI automatic prefix caching, Gemini
implicit caching) reuses the processing of requests that share the same byte
prefix. Putting mutating content in front would invalidate the whole history
on every change — a full-price reprocessing. At the tail, a change costs only
the block: new, uncached content anyway. And the block "sliding" one position
per appended history turn costs nothing: prefix matching only needs the
**leading portion** to agree, and the history is append-only.

### 13.1 `TodoContext` (`todo_ctxt.rs`)

A dedicated mini-manager for the plan (Cosh's `plan_todo_write` tool of the
`plan_*` family). The *harness* mirrors the tools' state via `set_todo_list`
at loop start and after every `plan_*` dispatch; `sync` stores the structured
list **and** memoizes the rendering (`tokens()` is read on **every** budget
check — re-rendering + re-estimating per call would be pure waste; the text
only changes when the list changes).

Rendering: a `## Tool TODOs` header with the update instruction, and one line
per task: `- [ ]` pending, `[*]` in progress, `[x]` completed, `[-]`
cancelled, with `(depends: …)` when dependencies exist.

**Removal rules** (the only two ways the block leaves the context):

1. **All tasks are terminal** — nothing `Pending`/`InProgress` remains
   (completed and cancelled both count as terminal): the plan is over.
2. **The model empties the list** — a `plan_todo_write` with an empty list:
   nothing to show.

**Protection**: the block is **not** a `ContextItem` — it is re-rendered from
the structured projection on every `build_messages`. Therefore: the
useless-chain sweep never touches it (not an item), and **not even the LLM
compaction** — which hides the entire pre-existing timeline — reaches it,
because it is re-rendered from the mirror afterwards. Its tokens **count**
toward the budget (the 80% trigger stays honest), but nothing can ever compact
it away.

For persistence, what travels is the **structured projection** (`todo.list()`),
not the rendering; `restore_state` syncs or clears depending on whether the
snapshot has a plan, and signals `todo_restore_pending` for the consumer-side
rehydration.

### 13.2 The correction block (`set_correction_block`)

The *harness*' tool-correction memory (`CorrectionMemory`) **churns** on every
tool failure — new entry, duplicate reordering, eviction. That is why it
**cannot** live in the system prompt: the prompt sits at position 0, and any
change there would invalidate the cache of the **whole** history — exactly the
mistake of the retired deterministic masking. Mirrored into the
`ContextManager` via `set_correction_block` and rendered at the tail
**before** the TODO block, a change costs the block itself. Never persisted;
never compacted; blank text clears the block.

The tail order is: [corrections] → [TODO] → (merged into the final `user`
turn, if any; blocks first, steering after).

---

## 14. The one-shot LLM compaction

### 14.1 The full flow

```
run() ── total ≥ 80% ──► RunOutcome::NeedsLlmCompaction
        harness: llm_compaction_request()          (ready-made prompt, no I/O)
        harness: summarization_request_overflow()  (does the prompt fit the summarizer window?)
              ├─ no  → calls the dedicated summarizer → apply_llm_summary(summary)
              └─ yes → escalates the contingency (MapReduce → split)
        apply_llm_summary → atomic validation → commit_summary_to_source
```

### 14.2 Source selection: `checkpoint_source_indices`

What enters the checkpoint:

1. **All visible items** (the base selection also excludes `Error`,
   display-only), sorted with **checkpoints first**
   (the checkpoint is the base state, even though it physically comes after
   the tail it precedes in the model's view);
2. **Pending inputs stay out**: the visible `User` turns at the end of the
   timeline (including queued steering that arrived after the most recent
   tool result) remain verbatim — compacting the question that has not been
   answered yet would lose the thread. The exclusion only holds when there is
   prior work (`has_prior_work`); in an inputs-only context, everything
   enters;
3. **Manual compaction**: returns everything (the explicit whole-session
   semantics of `/compact`);
4. **Automatic**: cuts the **raw tail** — `recent_raw_start_for` accumulates
   items back to front until ~`RECENT_RAW_PCT = 20%` of the budget (skipping
   `Compaction` and hidden items) — and the source is everything **before**
   it (plus the checkpoints, which can sit anywhere);
5. **Tail larger than its reservation** (pair-preserving expansion, or one
   giant single item): the whole tail is folded — `return visible` — instead
   of mutating provider-signed tool calls or installing a checkpoint that
   would relieve nothing;
6. **Pathological one-item context**: if the source came out empty and the
   total is already ≥ 80%, preserving a tail would leave nothing to compact —
   the source becomes everything (via `std::mem::take`).

`recent_raw_start_for` has one extra **structural integrity** rule: a result
retained in the tail must keep its matching call (otherwise the native pairing
breaks). Parallel calls can leave several results after the boundary; one
extra pass expands the tail back to the oldest required call — a pair is never
cut.

### 14.3 The prompt: `llm_compaction_request()`

Guards: empty timeline or total **below** the trigger → `None` (defensive; the
*harness* only calls this after `NeedsLlmCompaction`). Empty source → `None`.

The content is the selected old prefix, serialized in the opencode-style
format by `serialize_item`:

- `[User]: …`, `[Assistant]: …`, `[Assistant tool call]: name(args)`,
- `[Tool result]: …` **verbatim** — the tools bound their own output;
  truncating a payload the model still needs would only force a costlier
  re-read of the source,
- `[Previous summary]: …` + `[Covered source ranges]: …` for checkpoints —
  exposing the coverage is what allows a future reducer to **reread** the
  original items,
- `Error`/`UserCommand` serialize to the **empty string** — display-only never
  reaches a summarizer.

A masked result is serialized with its **original payload**: masking is a view
optimization, never data loss.

**Update mode**: if the source contains a `Compaction`, the prompt switches
from "create a new summary" to "update the summary labeled `[Previous
summary]` at the top of the transcript below, preserving still-true details,
removing stale details, merging in the new facts". The previous summary is
**not embedded in the instruction** — it stays in the timeline as the first
line of the transcript, a single copy, exactly where the timeline naturally
starts. This avoids *summary-of-summary* degradation: quality degrades when
summaries are blindly chained; updating an anchor with the delta is stable.

The `LlmCompactionRequest` carries `system` (the `SUMMARIZER_SYSTEM` — the
summarizer is a dedicated agent with its own instructions, **never** the agent
loop's system prompt from `core.rs`) and `prompt` (instruction + template +
transcript). The `SUMMARY_TEMPLATE` is structured opencode-style:
`## Objective`, `## Important Details` (with `Constraints`, `Decisions and
Evidence`, `Invalidated State`), `## Work State` (`Completed`, `Active`,
`Blocked`), `## Next Move`, `## Relevant Files`, `## Code & Anchors` — with
hard rules: keep every section, cite source item IDs, preserve paths /
hashline anchors (`¶path#TAG`) / lines / symbols / commands / errors **exactly**,
carry code **verbatim** in fences, preserve conflicts explicitly, and never
mention the compaction process.

### 14.4 Atomic application: `apply_summary_to_source_with_trigger`

Every application (one-shot, MapReduce, legacy split) goes through this
funnel:

1. Empty source or blank summary → `false`, nothing changed.
2. **The annotation comes before validation**: the exact text that will be
   installed is the annotated one — validating the unannotated candidate first
   would allow an over-budget commit (or reject a fitting one), because the
   annotation can grow or shrink the handoff.
3. **Projected validation**: `cached − removed + estimate(summary) +
   todo.tokens ≥ trigger` → **rejected** without touching anything. A
   degenerate summary is not a state transition; the current model view stays
   intact so failure and retry are genuinely atomic. (The legacy split passes
   a different trigger — `window + 1` — because it is driven by a window
   overflow, not the 80% trigger; §16.)
4. **Commit** (`commit_summary_to_source`):
   - `coalesce_ranges(direct_ids)` — the source's direct IDs become ranges;
   - every folded checkpoint's `covered_ranges` are **inherited** and
     everything is `merge_ranges`-ed — the final coverage describes exactly
     the set of visible source items the checkpoint replaces;
   - the covered IDs leave `masked` (the content is folded into the summary;
     keeping the mask would be noise);
   - the `Compaction { id, summary, covered_ranges }` is appended;
   - `checkpoint_coverage` receives the new coverage;
   - `recompute_cached_tokens()` — the source items are now hidden; the
     recompute reflects that in one step (non-local coverage: cheaper to
     recompute than to track each delta);
   - `clear_overflow()` — a successful compaction releases a stuck overflow.

### 14.5 The annotation hook and `/compact`

`set_summary_annotator` installs the *harness*' closure; `None` leaves
summaries untouched. The user's `/compact` sets `manual_compaction` (trigger
0), runs the same flow, and the idempotent `end_manual_compaction` closes it.
The manual pass selects the full source (§14.2, step 3) — the explicit
"whole session" semantics.

---

## 15. The hierarchical MapReduce contingency

When the **one-shot compaction prompt** itself does not fit the summarizer's
window (`summarization_request_overflow`), the *harness* escalates to
MapReduce (`checkpoint_context` → `map_reduce_context`): the timeline is
summarized into **independent segments** (*map*), the summaries are
**reduced level by level** down to a final candidate, the candidate is
**audited**, corrected if needed, and only then **committed**.

Structuring principle (module doc): **the staging stores only references and
derived summaries. The raw text is regenerated from the `ContextManager`'s
immutable item history for every request** — MapReduce never becomes a second
authoritative copy of the transcript.

### 15.1 `MapReduceState` — the persistent staging

| Field | Role |
|---|---|
| `version` | `MAP_REDUCE_VERSION = 3`. Staging from older versions (byte-sliced) is **discarded**, never executed. |
| `window`, `map_target_tokens`, `model` | The summarizer window the calls are sized against; the soft packing target; the model that took over the staging. |
| `source_item_ids` | The whole IDs of the frozen source (from the normal `checkpoint_source_indices` selection — the raw tail belongs to the receiving agent, it is not an input to the small summarizer; covered by a test). |
| `source_fingerprints` | `Vec<(id, xxh64)>` of each source item's `serialize_item`. **Fingerprinting of the frozen projection**: IDs can survive a revert or an item replacement; stale staging must never cover changed content under the same ID. |
| `segments` | The `MapSegment`s (ordinals, `item_ids`, `covered_ranges`, `summary: Option`). New staging **never uses byte slices**. |
| `phase` | `Mapping → Reducing → Validating → Correcting → Ready`. |
| `level`, `nodes`, `next_nodes`, `reduce_cursor` | The level-by-level reduction state: current-level nodes, the next-level accumulator, the cursor within the level. |
| `candidate`, `validation_cursor`, `audits`, `final_summary` | The final candidate and the audit cycle. |
| `parallel_disabled`, `repartitions`, `corrections` | Dynamic limits: a provider rejected concurrent requests; the repartition budget (max 3); the correction budget (max 2). |

### 15.2 Packing: `group_map_items`

The source items are grouped into **whole** segments (never sliced) under a
soft budget:

```
map_target_tokens = call_payload_budget(window, MAP_CALL_OVERHEAD = 8_000)
call_payload_budget(w, ov) = max(w − max(ov, 1).min(w/2), 1)
```

The packing rule is classic **sequential bin-packing with indivisible items**:
an item that does not fit in the current group opens the next group; the
**first item of a group may exceed the target** — items are not cut, and the
request preflight checks the actual window including overhead. This is exactly
the tested behavior: `packing_keeps_whole_items_and_resets_the_next_group_budget`,
`item_exceeding_remaining_space_moves_intact_to_the_next_group`,
`truly_oversized_singleton_is_never_sliced_or_retried_unchanged`.

Every segment carries `covered_ranges` (the coalesced ranges of its IDs) — the
provenance that traverses all levels.

### 15.3 The `Mapping` phase

- `pending_map_requests()` produces one `MapRequest` per segment without a
  summary, with `MAP_SUMMARIZER_SYSTEM` ("you do not see neighboring
  segments", "use only facts supported by the range", "record contradictions
  under Unresolved Conflicts") and an output target of
  `(window/10).clamp(128, 2000)` tokens.
- **Parallelism**: while `Mapping` and `!parallel_disabled`, the *harness*
  fires maps in parallel; a provider that rejects concurrent requests sets
  `parallel_disabled` — **already-accepted maps remain valid** and the rest
  run sequentially on resume.
- `accept_map_summary(ordinal, summary)` is **idempotent**
  (`segment.summary.is_none()` before writing — the
  `accepted_map_results_are_idempotent…` test guarantees a retry does not
  overwrite). When **all** segments have summaries, the nodes are promoted and
  the phase becomes `Reducing`.
- **Bounded repartition** (`repartition_incomplete_maps`): after a map
  overflow, incomplete segments are regrouped at whole-item boundaries with a
  reduced budget (a requested smaller window → the new window's budget;
  otherwise 3/4 of the current target); **accepted summaries stay attached to
  their original ordered ranges**; a singleton whose regrouping made no
  progress is split **in half by items** (never inside an item); if the count
  of incomplete segments did not shrink, everything is reverted and `false` —
  no loop against a bad window. Budget: `MAX_REPARTITIONS = 3`, persisted
  (restarts do not reset the counter against the same window).

### 15.4 The `Reducing` phase

`next_reduce_request()` groups the current level's nodes within
`call_payload_budget(window, REDUCE_CALL_OVERHEAD = 6_000)`; every node is
rendered as `[Covered ranges: #a-#b, …]\nsummary`. The request carries
`level`, `start/end/total`, `final_group` (when the whole level fits in a
single call) and the group's merged ranges.

`REDUCE_SUMMARIZER_SYSTEM` defines the contract: reconcile the complete
ordered set (deduplicate, remove invalidated, never promote proposals to
completed), and — crucially — **if an important conflict cannot be resolved,
output only `REREAD: start-end,…`** using IDs from the covered ranges.

- Non-final group → a new `SummaryNode` into `next_nodes`; when the cursor
  exhausts the level, `nodes ← next_nodes`, cursor 0, `level += 1`. Reduction
  is a **tree**: levels succeed until the final group. Hard limit:
  `MAX_REDUCTION_LEVELS = 8` — preventing the non-shrinking-output loop
  (tested: `reduction_limit_prevents_non_shrinking_model_output_loops`).
- Final group (the whole level fits in one call) → `candidate` set, phase
  `Validating`.
- **Cursor guards on acceptance**: `accept_reduce_summary` rejects a stale
  response when `state.level != request.level` or
  `state.reduce_cursor != request.start`; `accept_validation` when
  `validation_cursor != request.start`. That discipline is what makes the
  staging resumable and idempotent: a retry of an old call is never applied
  twice or out of order.

### 15.5 Reread: conflicts resolved with raw source

`parse_reread_request(response)`:

1. requires the literal `REREAD:` prefix; anything else → empty;
2. rebuilds the allowed coverage: the coalescing of `source_item_ids` **plus**
   the ranges of any `Compaction` present in the source (rereading evidence
   **behind a previous checkpoint** is allowed — the raw items stay immutable
   in the timeline);
3. filters availability: an ID must exist among the current items — **a
   removed item (revert) must not be silently represented by its neighbor**
   in a requested range;
4. limits: at most `MAX_REREAD_RANGES = 3` ranges, all contained within an
   allowed range; anything invalid → **vetoed atomically** (nothing partial is
   accepted).

`conflict_reduce_request(request, ranges)` re-renders the **raw** ranges
(`[Immutable source range #a-#b] …`) and appends to the reducer's prompt the
instruction to resolve the conflict and return the final reduction **without
requesting another reread**. A range whose text exceeds
`reduction_budget / 3` rejects the whole reread: **a partial excerpt cannot
establish which side of a conflict is correct** — truncating the evidence and
presenting it as complete would be worse than failing.

### 15.6 The `Validating` and `Correcting` phases

`next_validation_request()` audits the candidate against the map summaries, in
batches that fit in `reduction_budget − estimate(candidate)`.
`VALIDATOR_SYSTEM`: check that user constraints, decisions, invalidations,
completed vs. open work, blockers and the immediate next action survived
accurately; output **exactly `PASS`** or terse correction instructions
supported by the maps.

- All `PASS` → `final_summary = candidate`, phase `Ready`.
- Any non-`PASS` audit → phase `Correcting`.
- `correction_request()` builds the correction prompt (full template +
  candidate + numbered audits). Capped at `MAX_CORRECTIONS = 2` — repeatedly
  failing corrections **never commit** and are bounded (tested).
  `accept_correction` clears audits/cursor, increments the counter and returns
  to `Validating` — **every correction is audited again**; and it **rejects**
  a "summary" that starts with `REREAD:` — the reread protocol belongs to the
  reducer, never to the corrector.
- Changing the staging's model (`prepare_map_reduce_model`) **resets the
  dynamic limits** (`repartitions`, `parallel_disabled`, `corrections`, and
  `level` if exhausted) — a larger model deserves a fresh chance; a smaller
  model re-sizes the window and the target.

### 15.7 Commit and abort

`commit_map_reduce()` requires: the source still **valid** (current version, no
slices, all source IDs present and visible, **fingerprints matching**), phase
`Ready`, `final_summary` present, and all source IDs mappable to indices. Then
the commit descends through the **same funnel** as the one-shot compaction
(`apply_summary_to_source`) — including the projected validation against the
**receiving agent's budget**, not the summarizer's window (explicitly tested:
`map_reduce_commit_uses_the_receiving_agents_budget`). Failure of any
precondition: **the staging comes back intact** (resumable for a later
recovery) and nothing in the view changed.

`abort_map_reduce()` discards the staging.
`discard_unverified_compaction_staging` discards old-version staging (map or
split) and releases the overflow — source events and already-committed
checkpoints are **never** altered by the migration; `begin_map_reduce` over
stale staging rebuilds from scratch.

`begin_map_reduce(window)` over active staging: a **larger** window resumes
the accepted work (the window is `min(current, new)`); smaller windows keep
the original references and repartition on overflow. Re-entering **never**
discards accepted work.

`map_progress() -> (done, total)` feeds the progress indicator; the reduction
*level* continues afterwards.

### 15.8 The query surface used by the *harness*

Beyond the cycle above, the *harness* consults the contingencies' state
through dedicated accessors, all read-only: `map_reduce_active()`,
`compaction_staging_active()` (MapReduce **or** split in progress — the
*harness* uses it to route a second compaction request into the in-progress
checkpoint instead of starting another one), `map_reduce_window()`,
`parallel_mapping_enabled()` / `disable_parallel_mapping()` (the §15.3
parallelism control, exposed to the dispatch engine), `split_active()` and
`split_all_consumed()` — the latter returns `true` also when there is **no
staging**, which makes `commit_split` safe to call without a pre-check. On the
core side, `todo_list()` (the plan's structured projection), `max_tokens()`
(the budget, used to size the contingency when the real window is unknown),
`has_compactable_content()` and `display_info()` complete the external read
of the state.

---

## 16. The legacy contingency: split-and-concatenate

`split.rs` is MapReduce's predecessor, kept for a single reason: **staging
already persisted by older versions must remain resumable after an upgrade** —
the `SplitState` in the snapshot has to stay executable. New code uses
MapReduce.

The algorithm: when the one-shot compaction itself overflows the window, the
timeline is summarized in **sequential chunks**; each chunk continues where
the previous one ended; the summaries are **concatenated into ONE final
anchor**.

Constants and their roles:

| Constant | Value | Role |
|---|---|---|
| `SPLIT_BUFFER_MAX_PCT` | 0.40 | The buffer's **ceiling**: 40% of the model window. |
| `SPLIT_WARN_PCT` | 0.80 | The fraction **of the ceiling** where the limit message starts being sent to the summarizer. |
| `SPLIT_CONTINUITY_CHARS` | 1200 | How many trailing characters of the previous summary travel into the next chunk as the continuity snippet. |
| `SPLIT_CALL_OVERHEAD` | 8_000 | Fixed overhead reserved per call (system + template + continuity + metadata + output headroom). |
| `VERIFIED_SPLIT_VERSION` | 1 | Staging without completion verification (version 0) cannot commit. |

Cycle: `begin_split(window)` → `split_next_chunk()` → the *harness* calls the
summarizer → `advance_split(summary, chunk_end)` → … → `commit_split()` /
`abort_split()`. `begin_split` is **re-entrant**: calling it with a staging
already active only updates the window — buffer and cursor are preserved
(resume semantics, the analogue of MapReduce's `begin_map_reduce`).

- **`split_next_chunk`** — chunks of **whole** items in historical order
  (visible items only; hidden ones "are not there"), sized to
  `window − overhead` with the overhead **capped at half the window** (a small
  window — a small fallback model — keeps a usable chunk budget instead of
  degrading to one item per chunk). The first item of the chunk enters
  **whole** even when it alone exceeds the budget: structural integrity beats
  budget precision. The **cursor only advances in `advance_split`** — a
  failed chunk can be retried identically.
- **Continuity**: the first chunk carries the full template (producing the
  anchored skeleton); the following ones receive the last 1.200 characters of
  the previous summary and the instruction to **extend** — never restart the
  template — so the final buffer reads as **one continuous document with no
  visible seam**.
- **Early containment**: below 80% of the ceiling the model acts naturally (a
  long-session summary rarely approaches the ceiling — "observe and only warn
  when it matters"); above that, the prompt receives the proportional target
  `⌈chunk_tokens · available / remaining⌉` (minimum 64), so the final buffer
  fits under the ceiling with the slack built into it. The warning text
  includes how much has been summarized and how much remains — real context,
  not a magic number.
- **Incremental accounting**: `buffer_tokens` adds only the new summary (+
  the `\n` separator); re-encoding the whole buffer per chunk would be
  O(total²) across a long split. The estimate is **additive** (per parts) and
  may run a few tokens above the exact value — the conservative direction
  (warns slightly earlier), and the **commit re-estimates the whole buffer**,
  never depending on this estimate.
- **`commit_split`** — atomic and with its own success criterion: only when
  **every item has been consumed**, and via `apply_full_llm_summary` (the full
  visible source, trigger `window + 1`). The success criterion is the
  **window**, not the 80% trigger: the split is driven by a model-window
  overflow; an anchor well under 80% of the budget is still a valid commit.
  The buffer is re-estimated whole here — a pathological overshoot (an anchor
  larger than the window) **rejects without changing anything** (the staging
  comes back; the *harness* marks the stuck overflow and notifies). Note:
  since the split already summarized the raw tail, the commit uses the full
  source — retaining the tail would duplicate it in the model's view.
- **`abort_split`** — discards buffer + cursor; the timeline was **never**
  touched; the conversation stays exactly as it was.

---

## 17. Stuck overflow, window discovery and the error catalog

### 17.1 Stuck overflow (`overflow_model`)

When a window overflow occurs and **no** contingency can relieve it (one-shot
overflows, the split does not fit), the manager records
`overflow_model = Some(model)`. While the same model is active:

- `overflow_stuck(model)` → true; the *harness* **skips the doomed summarizer
  call** (each attempt would be a paid call failing again) and merely
  **re-notifies** the user;
- `sync_model(model)` at every loop start: a different model → the user
  switched → `clear_overflow()` and a fresh chance from scratch;
- a successful compaction commit also releases it (`clear_overflow`).

The key being **model** and not provider follows directly from what is stuck:
the window is a property of the model.

### 17.2 `record_provider_window`

A provider overflow is the **provider's own report** of its maximum input
size ("maximum context length is 128000") — more authoritative than any public
catalog. The method: resizes the budget to the reported window's **effective**
value (or the full window) and persists the **raw** window into the error
catalog, so future launches discover it **before** paying another overflow.
Skipped under `cfg(test)` — tests run against the real data directory and must
never write to it.

### 17.3 The catalog (`error_catalog.rs`)

`~/.local/share/cosh/api_errors.json` — a `model → window` map. Deliberate
decisions:

- **Location**: at the top level of the data dir, **not** under the `cache/`
  subdir of the public catalogs — it is a **record**, not a cache: private
  per-machine state of what the user's own provider observed, deliberately
  isolated from `cosh-sdk`'s discovery (which lists public models).
- **Atomic write**: temp file in the **same directory** (rename stays on one
  filesystem) with a **PID** suffix (two concurrent instances never
  interleave write/rename over a shared temp path), `write_all` + `sync_all`
  **before** the rename (a rename published without durable content could
  survive a crash pointing at garbage), and temp cleanup on any failure.
  **Never fatal** — a cache-write failure must not block the agent loop.
- **Tolerant read**: missing or corrupted file → `None`, ignored, never fatal.
- **Flexible matching**: exact, vendor-less suffix (`openai/gpt-4o` ↔
  `gpt-4o`), case-insensitive, in both directions — mirroring the SDK
  discovery's matching.

---

## 18. `ContextManagerState`: the transport projection

```rust
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub max_tokens: usize,
    pub overflow_model: Option<String>,
    pub split: Option<SplitState>,
    pub map_reduce: Option<MapReduceState>,
    pub visible_from: Option<u64>,
    pub hidden: HashSet<u64>,
    pub masked: HashSet<u64>,
    pub todo: Option<TodoList>,
    pub last_tool_set: Option<Vec<String>>,
}
```

This is the module's **transport contract** — carried by the *harness* events
(`Done`/`Stopped`/`Error`/`ContextSnapshot`) out of the module. Three
properties matter (the UI-side persistence mechanism is out of scope for this
document):

1. **Nothing is in-flight between save and restore** — the snapshot is a plain
   clone; restoring is assigning field by field.
2. **It is never an authoritative on-disk snapshot** — it is the transport
   projection the receiver compares and derives differences from. The timeline
   itself is append-only and immutable.
3. **Staging is real progress**: `split` (buffer + cursor) and `map_reduce`
   (references + derived summaries, phases, cursors) resume exactly where they
   stopped.

Beyond the copy, `restore_state` performs **hygiene**:

- `masked.retain(only live result IDs)` — a mask for a result that no longer
  exists is removed;
- `rebuild_checkpoint_coverage()` — coverage is derived from the items, never
  trusted from the persisted state;
- `todo` — `Some` syncs, `None` clears explicitly; and
  `todo_restore_pending` is signalled for the rehydration;
- `recompute_cached_tokens()` — the items were replaced wholesale; the cache
  is rebuilt.

Fields deliberately **not** persisted: `correction_block` (transient,
re-mirrored every request), `manual_compaction` (the *harness* brackets the
manual pass), `checkpoint_coverage` (derivable), `todo_restore_pending`
(a signal, not state), `cached_items_tokens` (derivable), `encoding`
(re-derived by `set_model` at every loop start), `binding_tx`
(plumbing), `summary_annotator` (the *harness* reinstalls it with a fresh
snapshot at construction).

`last_tool_set` persists so the tool-drift detector works **across** restored
turns.

---

## 19. Mathematical precision: removing the simplifications

Some quantities were described loosely above ("~20%", "80%"). This section
closes the accounts precisely.

### 19.1 The effective window (ECW)

The advertised window (MCW, *maximum context window*) is not the window the
model actually reasons over (MECW, *maximum effective context window*). Cosh
uses the logarithmic decay curve (Paulsen 2025, §21):

$$
\mathrm{frac}(W) \;=\; \operatorname{clamp}\bigl(A - B\log_{10} W,\; f_{\min},\; f_{\max}\bigr)
\qquad
\mathrm{ECW}(W) \;=\; \operatorname{round}\!\bigl(W \cdot \mathrm{frac}(W)\bigr)
$$

with $A = 2.612$, $B = 0.443$, $f_{\min} = 0.20$, $f_{\max} = 0.85$.

- **Why logarithmic?** Small windows are genuinely usable almost whole
  ($\mathrm{frac} \to f_{\max} = 0.85$ by the cap); giant windows are strongly
  limited ("the bigger the window, the bigger the illusion"). The product
  $W\cdot\mathrm{frac}(W)$ still **grows** with $W$ — the fraction decays
  sublinearly.
- **Why a floor of 0.20 and not lower?** Monotonicity of the product requires
  $W\cdot\mathrm{frac}(W)$ to be increasing:
  $$
  \frac{d}{dW}\Bigl[W\,(A - B\log_{10} W)\Bigr] \;=\; A - B\log_{10} W - \frac{B}{\ln 10}
  \;>\; 0
  $$
  is guaranteed whenever $\mathrm{frac}(W) = A - B\log_{10}W >
  B/\ln 10 \approx 0.443/2.3026 \approx 0.1924$. The floor $0.20 > 0.1924$
  keeps the whole function monotonic: **a larger advertised window never
  yields a smaller effective window** — without the floor, products beyond
  the line $W > 10^{(A - 0.1924)/B}$ would start *decreasing*.

The manager's budget is $\mathrm{ECW}(W)$ when real discovery succeeds;
$\texttt{MAX\_CONTEXT\_TOKENS} = 100\,000$ otherwise.

### 19.2 The call-payload budget

$$
\mathrm{budget}(w, o) \;=\; \max\bigl(w - \min(\max(o,1),\,\lfloor w/2 \rfloor),\; 1\bigr)
$$

The overhead is **capped at half the window**: a small-window summarizer model
cannot have its payload squeezed to almost nothing by the fixed overhead (the
"one item per chunk" degradation of §16). Used with
$o_{\text{map}} = 8\,000$, $o_{\text{reduce}} = 6\,000$ and
$o_{\text{split}} = 8\,000$.

Derived targets:

$$
T_{\text{map}} = \mathrm{budget}(w, 8\,000), \qquad
T_{\text{reduce}} = \mathrm{budget}(w, 6\,000),
$$
$$
T_{\text{out,map}} = \operatorname{clamp}\!\left(\frac{w}{10},\; 128,\; 2\,000\right).
$$

### 19.3 The trigger and the tail

$$
\Theta \;=\; \left\lfloor \frac{80 \cdot \texttt{max\_tokens}}{100} \right\rfloor,
\qquad
R \;=\; \left\lfloor \frac{20 \cdot \texttt{max\_tokens}}{100} \right\rfloor .
$$

The raw tail is the largest suffix of visible (non-`Compaction`) items whose
sum $S$ obeys `recent_raw_start_for`'s stop rule: an item enters while
$S < R$ **and** $S + t_{\text{item}} \le R$ — the exact negation of the stop
`held >= target || held + tokens > target`; the first item of the tail is
exempt and always enters — followed by the **pair-preserving expansion**: for
every `call_id` with a retained result, the tail extends back to include the
call — formally, the final start is
$\min_{\text{retained } c} \mathrm{idx}(\mathrm{call}(c))$.

### 19.4 Split: ceiling, warning and target

$$
C \;=\; \lfloor 0.40\,w \rfloor \quad(\text{ceiling}), \qquad
\text{warn} \;\Longleftrightarrow\; B \;\ge\; \lfloor 0.80\cdot C \rfloor
\;=\; \bigl\lfloor 0.80\cdot\lfloor 0.40\,w \rfloor\bigr\rfloor \;\approx\; \lfloor 0.32\,w \rfloor,
$$

$$
B_{k+1} \;=\; B_k + t(\text{summary}_k) + [\,k>0\,]\cdot t(\text{"\textbackslash n"}),
\qquad
\text{target}_k \;=\; \max\!\left(64,\; \Bigl\lceil c_k \cdot \frac{C - B_k}{\rho_k}\Bigr\rceil\right),
$$

where $B_k$ is the buffer before chunk $k$, $c_k$ the chunk's tokens and
$\rho_k$ the tokens remaining after it. The denominator $\rho_k$
**proportionalizes** the target: each chunk gets the slice of the remaining
space proportional to its weight, so the final buffer stays under the ceiling
with the slack already built into it (40% ≤ 100% of the window minus
overheads). The 64-token floor prevents degenerate targets on the last chunks.
The final validity is checked by **re-estimating the whole buffer** against
$w$ (not $C$) — the additive accounting $B_k$ only decides the warning and the
target.

### 19.5 The commit's projected validation

For a source $F$ with visible cost $\sum_{i \in F} t_i$, candidate $s$ and
trigger $\Theta'$:

$$
\text{commit} \;\Longleftrightarrow\;
\Bigl(\mathrm{cached} - \textstyle\sum_{i\in F} t_i\Bigr) + \hat t(s) + \hat t(\text{todo}) \;<\; \Theta',
$$

with $\Theta' = \Theta$ on the normal path and $\Theta' = w + 1$ on the split
(§16). The **strict** inequality is what guarantees that a successful commit
leaves the total **below** the trigger — the next `run()` iteration returns
`Resolved` instead of immediately re-entering compaction (the
compact-forever loop is the failure mode the *wedge* tests exercise).

### 19.6 Complexity

- `total_tokens()`: **O(1)** (cache + two memoized blocks); the brute-force
  O(n) only in debug builds.
- `build_messages()`: **O(n)** per request, a single pass (plus `is_hidden`
  O(1) per item).
- `sweep_useless_chains` / the abandonment sweeps: O(n) per tick, with O(1)
  hash lookups; quiescence within at most O(n) degenerate passes.
- MapReduce packing: **O(n²)** per repartition — `group_map_items` calls
  `item_map_tokens(id)`, which does a **linear** search of the timeline per
  item ID; same for `render_segment`. With at most 3 repartitions the factor
  stays bounded, but the linear search per item is the real cost.
- `hide_item`: O(n) worst case (partner search) — and the first pass of
  `sweep_useless_chains` that hides k chains costs O(k·n) through it (the
  following passes are O(n): already-hidden chains are inert). Removals are
  rare and bounded, by design.

---

## 20. Tests: what the module guarantees

`src/harness/context/test.rs` (plus the internal `mod tests` of `todo_ctxt`,
`error_catalog` and `map_reduce`) — **109 tests**, gate
`cargo test --lib harness::context::` (108 run, 1 ignored). The groups:

- **Ingestion and rendering** — native roles (`user`/`assistant`/`tool`),
  1:1 item→message, steering dedup, thinking-block round-trip, leading
  orphaned tool results dropped.
- **`Closure`** — promotion, the guard (last item not closable → nothing),
  the `reasoning` carry-over, `final_answer`.
- **Chains and sweeps** — the native pair intact, the useless-chain sweep
  (including the newest-chain protection and the mask exception),
  cancellation debris (orphans + abandoned inputs), hidden commands invisible
  to the model and to the budget.
- **Budget** — the 80% trigger, relief by masking and by sweeping, and the
  canonical `cached_token_total_stays_in_sync_across_all_mutations` test (the
  drift invariant, exercised across all mutations).
- **Compaction** — full serialization, update mode (never duplicating the
  summary), `None` below the trigger, atomic rejection, the raw tail
  preserved, pending inputs verbatim, `/compact`.
- **MapReduce** — the tail retained independently of the summarizer window,
  the receiving agent's budget at commit, whole-item packing, the giant
  singleton never sliced, invalid/old-version staging rebuilt from source,
  repartition without shrinking the real window, map idempotence and resume,
  recursive reduction with progress, bounded reread and behind-checkpoint
  reread, validation/correction as commit gates, oversized-checkpoint
  rejection without changing the view, source-replacement invalidation,
  correction and level limits, queued inputs verbatim.
- **Split** — whole chunks, the cursor advancing only on success, warning
  only under concerning accumulation, atomic commit with whole-buffer
  re-estimation, staging surviving save/restore and resuming.
- **Snapshot** — full round-trip, `masked` hygiene, coverage rebuilt.
- **Discovery** — `from_window` (the fallback never resized), effective
  resolution, the tolerant and atomic error catalog.

Running the suite is the recommended way to check any behavior in doubt: the
module is self-contained and the tests **do not depend on the network by
default** — the only test that touches the network (real window discovery) is
marked `#[ignore]` precisely for that.

---

## 21. References and further reading

### Direct references (used in the code's calculations and decisions)

1. **Paulsen, B. (2025). *Context Is What You Need*.** arXiv:2509.21361 —
   the empirical basis of the effective-window curve (§19.1): the usable
   window grows sublinearly with the advertised window; the SDK's comment
   cites the paper directly ("the bigger the window, the bigger the illusion").
   <https://arxiv.org/abs/2509.21361>
2. **Dean, J. & Ghemawat, S. (2004). *MapReduce: Simplified Data Processing on
   Large Clusters*.** OSDI'04 — the map→reduce→reconcile skeleton of §15,
   transplanted to LLM calls with level-by-level reduction trees.
   <https://research.google/pubs/mapreduce-simplified-data-processing-on-large-clusters/>
3. **Anthropic — Prompt caching** (`cache_control`, prefix-based cache) —
   <https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching>
4. **OpenAI — Prompt caching (automatic prefix caching)** —
   <https://platform.openai.com/docs/guides/prompt-caching>
5. **Google — Gemini implicit caching** —
   <https://ai.google.dev/gemini-api/docs/caching>
6. **OpenAI — tiktoken** (and the `tiktoken-rs` crate) — the module's token
   estimates; `cl100k_base` as the cross-provider default.
   <https://github.com/openai/tiktoken>
7. **opencode** — the declared inspiration of the summary template
   (`SUMMARY_TEMPLATE`, "opencode-style") and of the summarizer-agent split.
   <https://github.com/sst/opencode>
8. **APIs with restricted replay** — Claude *extended thinking* (signed blocks
   must be replayed verbatim) and Gemini *thought signatures* (`functionCall`
   rejects replay without the signature):
   <https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking> ·
   <https://ai.google.dev/gemini-api/docs/thinking>

### Further reading (context for the techniques used)

- **Log-structured / append-only storage**: the timeline is an event *log*
  with a derived *view* — the same pattern as event sourcing and LSM-trees
  (tombstone markers instead of physical deletion).
  O'Neil et al., *The Log-Structured Merge-Tree*, Acta Informatica (1996).
- **Prefix caching as a design constraint**: the module's "tail-only
  mutations" rule is an instance of *write-optimized views over an immutable
  prefix* — the same trade-off that makes compilers avoid invalidating cached
  AST prefixes.
- **Tokenization and estimation**: providers' context limits are counted in
  the provider's own tokenizer; estimating with tiktoken introduces a small
  systematic error, which the design absorbs with the effective window (§19.1)
  and the 80% trigger (§19.3) — *safety margin* instead of false precision.
- **MapReduce with validation**: the map→reduce→validate→correct chain of §15
  is *self-refinement with external verification* — the independent audit
  (`VALIDATOR_SYSTEM`) is the mechanism that prevents committing a checkpoint
  with a material omission, and the hard limits (levels, corrections,
  repartitions) guarantee termination.
- **Atomicity via pre-commit validation**: the §14.4/§15.7 pattern (plan,
  validate the exact result, commit or do nothing) is optimistic
  transactionality: the "transaction" is the model's view, and the *rollback*
  is trivial because nothing was mutated.

### One-line summary per concept

- **Timeline**: append-only; visibility is projection, never deletion.
- **Budget**: O(1) memoized, counting only what the model would see.
- **Trigger**: 80% of the effective budget; manual = 0.
- **Cheap relief**: tail masking on the model's request; the useless-chain sweep.
- **Last resort**: an anchor checkpoint with exact coverage, atomic commit.
- **Contingencies**: hierarchical MapReduce with an audit; the legacy split
  only to resume old staging.
- **Always preserved**: user input verbatim, code verbatim in summaries, the
  TODO plan immune to everything, the full history for revert/fork.
