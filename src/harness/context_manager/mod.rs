//! Single-owner LLM-free synchronous context manager.
//!
//! The [`ContextManager`] is the **single owner** of the whole conversation —
//! user prompts, assistant outputs, tool calls and tool results — before it
//! reaches the LLM. Fresh content is rendered verbatim; the TF-IDF → LSA →
//! MMR compression pipeline runs **synchronously on the agent loop's thread**
//! as the first phase of the 80% compaction ([`compression::init`]). There is
//! no worker thread: no CPU is spent compressing anything unless the budget
//! actually overflows, and the compressed copy is swapped in at the **same
//! position** — a field update, so the message order is never reorganized.
//!
//! The most recent compressible assistant turn may be the agent loop's FINAL
//! output, which must keep its original text: [`close_loop`](Self::close_loop)
//! promotes the ORIGINAL text verbatim into a `LoopClosure` the moment the
//! loop ends, so the final answer is always delivered in its original form
//! even if a pipeline pass had already summarized the raw draft.
//!
//! [`build_messages`](Self::build_messages) renders the conversation as a
//! provider-ready `Vec<ChatMessage>` with a **1:1 mapping** between items and
//! messages, so every message keeps its original position by construction.
//!
//! # Compaction
//!
//! When the held context reaches 80% of the budget, the compaction runs. The
//! funnel is **pipeline-first by construction**: phase 1 (the pipeline) always
//! leads, and phase 2 (gradual draft eviction) only runs when the pipeline
//! could not bring the total below the trigger. The [`LoopClosure`] is the
//! shared progress checkpoint:
//!
//!   1. **The pipeline** (`pipeline_pass`) — the compression runs here,
//!      synchronously, on the agent loop's thread. It compresses every
//!      compressible assistant draft of the CURRENT segment (the drafts up to
//!      the next [`LoopClosure`]): below the trigger at the segment boundary
//!      → the pass is done; still over → the pass STOPS here (the parada por
//!      segmento) and phase 2 takes over the same segment — the next
//!      segment's drafts wait until the current one is exhausted;
//!   2. **Gradual draft eviction** (`evict_drafts`) — removes one whole
//!      assistant draft at a time, oldest-first, from a persistent cursor;
//!      after each removal the budget is checked and the pass stops the
//!      moment the total drops below 80%. The cursor resumes across
//!      consecutive overflows and the pass yields at a [`LoopClosure`] — the
//!      oldest segment's drafts are exhausted, and the funnel repeats into
//!      the next segment (see below). The [`LoopClosure`] is the shared
//!      CHECKPOINT of phases 1 and 2: both stop at it and continue after it.
//!
//! **User prompts, `LoopClosure`s, tool chains and previous compaction
//! summaries are protected** from both deterministic phases — they are never
//! prose-compressed, never evicted and never trimmed. The old anchor system
//! (a verbatim "first prompt of the segment") is gone: every user input keeps
//! its original text until the LLM compaction folds it into the general
//! summary.
//!
//! Because the pipeline always leads and phase 2 is bounded by the segment
//! frontier, eviction structurally never touches prose the pipeline could
//! still summarize. The guarantee comes from this ORDER, not from an
//! eligibility rule: if the pipeline breaks or times out on a draft, that
//! draft stays raw but remains evictable, so the system degrades gracefully
//! instead of deadlocking.
//!
//! The 1 → 2 alternation grinds through the drafts segment by segment INSIDE
//! a single [`run`](Self::run) — "tudo se repete": when phase 2 exhausts a
//! segment (reaching its closing [`LoopClosure`]) still over the trigger, the
//! pipeline immediately takes the next segment, then phase 2 drains it, and
//! so on until the total drops below 80% or every draft is gone. A **segment
//! frontier** tracks the start of the current segment: phase 1 compresses it,
//! phase 2 evicts it, and when phase 2 exhausts the segment's drafts the
//! frontier advances past the closure. Phase 2 can never cross the frontier
//! into a segment the pipeline has not processed. Across overflows, the
//! persistent cursor keeps the drain granular — one chunk per stop when the
//! budget allows — so the decoupling is spread over many dispatches.
//! When **no draft remains** (nothing for the pipeline to compress, nothing
//! for the draft pass to evict) and the total is still over the 80% trigger,
//! the manager returns [`RunOutcome::NeedsLlmCompaction`]: the harness then
//! runs the **LLM compaction** — the third level, OUTSIDE the toggle, a
//! last-resort fallback (opencode-style). The entire remaining context
//! (protected items included) is serialized and sent to the model, which
//! produces a continuation summary that replaces the whole timeline in place.
//! That is the only compaction that ever touches the protected items.
//!
//! A separate automatic cleanup, also outside the toggle, removes **useless
//! tool chains** (results marked `useless` — e.g. a zero-match `find_grep`)
//! at the start of every [`run`](Self::run): dead weight is dropped as soon
//! as the model has had one read of it, keeping the newest chain alive.
//!
//! Two deliberate floors keep the total above the trigger in rare cases, by
//! design: (1) the protected items (user prompts, closures, tool chains,
//! compaction summaries) are never removed by the deterministic phases, so if
//! they alone exceed the model window the total stays over budget until the
//! LLM compaction runs — and if that call fails, the context simply stays
//! over until it succeeds (the accepted fallback); (2) a segment whose drafts
//! are exhausted still over the trigger hands the pipeline the next segment
//! inside the same `run()` (the 1 → 2 grind repeats), so the alternation
//! converges to the 80% trigger or to `NeedsLlmCompaction` within a single
//! overflow.
//!
//! When the provider itself rejects the compaction because the serialized
//! context exceeds ITS window (a `ContextWindowExceeded` error), the harness
//! drains tool chains one at a time via [`Self::evict_tool_chain_for_overflow`]
//! (never inputs, closures or summaries) and retries; once every chain is gone
//! the overflow is recorded as stuck for that provider
//! ([`Self::mark_overflow`]) and the user is notified through the TUI until
//! they switch the model.

pub mod compression;
#[cfg(test)]
mod test;
pub mod todo_ctxt;

use crate::util::TokenEncoding;
use cosh_sdk::connector::{
    ChatMessage, ToolCallFunctionMsg, ToolCallMsg, assistant_tool_call_message,
    discover_context_window, tool_result_message, user_message,
};
use cosh_tools::plan::types::TodoList;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use text_splitter::TextSplitter;
use todo_ctxt::TodoContext;

use compression::init::{CHUNK_CAPACITY, init as deterministic_compress};

/// Default token budget for the total conversation context. Overridable via
/// [`ContextManager::new`].
pub const MAX_CONTEXT_TOKENS: usize = 100_000;

/// Percentage of the budget at which the compaction runs.
const COMPACT_PCT: usize = 80;

/// Retention preview: when the context is under budget pressure and the
/// drafts are exhausted, OLD tool results are trimmed to this many leading
/// characters instead of holding every full output for the rest of the
/// session (the growth that drives long-session OOMs). The item stays a valid
/// `tool` message — only the content is shortened.
const TOOL_RESULT_PREVIEW_CHARS: usize = 300;

/// Prefix that marks an already-trimmed tool result, so a later overflow
/// never re-trims it: the trimmed text stays bounded and stable across the
/// repeated `run()` calls of a busy tool loop.
const TOOL_RESULT_TRIM_MARKER: &str = "[trimmed, was ";

/// TF-IDF maximum document frequency filter for deterministic compression.
const TFIDF_MAX_DF: f64 = 0.8;

/// MMR lambda balancing relevance and diversity for deterministic compression.
const MMR_LAMBDA: f64 = 0.7;

/// MMR compression ratio: keep roughly this fraction of the input chunks.
const MMR_RATIO: f64 = 0.4;

/// Maximum number of sentence-chunks per deterministic compression pass.
/// Larger inputs are split into batches to bound the O(n²) SVD/MMR cost.
const DETERMINISTIC_MAX_CHUNKS: usize = 200;

// ── Split-and-concatenate (the known-window contingency) ────────────────
//
// When the ACTIVE model's window is KNOWN (discovery or a context-window
// error that reports it) and the held context exceeds that window — the
// model-switch-to-a-smaller-window scenario — the legacy chain drain cannot
// help (protected items are never evicted). Instead the harness drives the
// split-and-concatenate contingency: the timeline is summarized in
// sequential chunks (whole items, historical order), each chunk's summary is
// appended to a staging buffer, and — only when EVERYTHING has been
// summarized — the buffer is committed atomically as the new single anchor
// (exactly the end state of a normal LLM compaction). The timeline is never
// touched until that final commit; a failure at any point aborts and
// everything stays.

/// The final anchor buffer must stay at or below this fraction of the model's
/// window (the "ceiling" — the max capacity of the concatenation buffer).
const SPLIT_BUFFER_MAX_PCT: f64 = 0.40;

/// Fraction of the buffer ceiling at which the limit message starts being
/// sent to the summarizer. Below it the model is left to act naturally (a
/// long-session summary rarely approaches the ceiling); only when the
/// accumulation becomes concerning do we ask it to be more terse.
const SPLIT_WARN_PCT: f64 = 0.80;

/// How many trailing chars of the previous chunk summary are carried into the
/// next chunk as the continuity snippet, so the final buffer reads as one
/// continuous summary (no visible seam).
const SPLIT_CONTINUITY_CHARS: usize = 1200;

/// Fixed token overhead reserved per chunk call: the summarizer system prompt,
/// the split template/instructions, the continuity snippet, the metadata lines
/// and the model's output headroom. The serialized chunk itself is sized to
/// `window - overhead` so the whole request fits the window. The reserve is
/// capped at half the window (see [`ContextManager::split_next_chunk`]) so a
/// small window (a small fallback model) keeps a usable chunk budget instead
/// of degrading to one item per chunk.
const SPLIT_CALL_OVERHEAD: usize = 8000;

// Tool results are NOT truncated in the LLM-compaction transcript: the tools
// themselves bound their output (hashline-numbered reads, capped search
// results), and cutting a payload the model may still need forces it to
// re-read the source afterwards — spending more tokens on re-reading than
// the truncation ever saved. The summary prompt is bounded by the model's
// own window, and the existing overflow recovery drains tool chains if the
// transcript ever exceeds it.

/// The summarization template the LLM compaction asks the model to fill
/// (opencode's `SUMMARY_TEMPLATE`, kept as inspiration): a structured anchor
/// that preserves the objective, the work state and the next move so the
/// session can continue seamlessly from the summary. The `Code & Anchors`
/// section exists so the agent resumes editing where it stopped WITHOUT
/// re-reading whole files: hashline anchors (`¶path#TAG`) and the exact code
/// blocks the next step touches are carried forward verbatim.
const SUMMARY_TEMPLATE: &str = "\
Output exactly the Markdown structure shown inside <template> and keep the section order unchanged. Do not include the <template> tags in your response.
<template>
## Objective
- [one or two brief sentences describing what the user is trying to accomplish]

## Important Details
- [constraints/preferences, decisions and why, important facts/assumptions, exact context needed to continue, or \"(none)\"]

## Work State
### Completed
- [finished work, verified facts, or changes made; otherwise \"(none)\"]

### Active
- [current work, partial changes, or investigation state; otherwise \"(none)\"]

### Blocked
- [blockers, failing commands, or unknowns; otherwise \"(none)\"]

## Next Move
1. [immediate concrete action, or \"(none)\"]
2. [next action if known, or \"(none)\"]

## Relevant Files
- [file path with its hashline anchor (¶path#TAG) and the line ranges that matter, then why it matters; or \"(none)\"]

## Code & Anchors
- [code blocks, function signatures, hashline anchors (¶path#TAG) and line numbers the next step needs to resume without re-reading whole files; otherwise \"(none)\"]
</template>

Rules:
- Keep every section, even when empty.
- Use terse bullets, not prose paragraphs — except code, which is ALWAYS copied verbatim inside Markdown code fences.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers, symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures and error messages exactly as they appear in the transcript. Never paraphrase code — a paraphrased block cannot be applied or edited.
- The summary must let the agent resume editing where it stopped: for every file the next step touches, keep the hashline anchor of its last read/written state and the specific lines/symbols involved.
- Do not mention the summary process or that context was compacted.";

/// The SYSTEM prompt of the LLM summarizer — its OWN instructions, completely
/// separate from the agent loop's fixed system prompt in `core.rs`. The
/// summarizer is a dedicated agent with its own context and its own rules
/// (opencode-style): it never sees the main system prompt, the tool
/// definitions, or the harness instructions.
const SUMMARIZER_SYSTEM: &str = "\
You are a conversation summarizer for an AI coding agent.
You read the transcript of everything that happened in the session and produce
a single anchored Markdown summary that lets the session continue seamlessly.
Rules:
- Follow the template exactly; never add sections, never change their order.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers, symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures and error messages exactly as they appear in the transcript (inside fenced blocks). The agent edits files by hashline anchor and line number — paraphrased code cannot be used.
- Prefer a few exact code blocks over prose descriptions of the same code.
- Keep facts precise; do not invent details that are not in the transcript.
- In update mode, preserve still-true details, remove stale details, and merge in the new facts.
- Do not mention the summary process or that context was compacted.";

/// A single conversation item in display order. **One item = one message** in
/// [`ContextManager::build_messages`], so message positions are preserved by
/// construction and a compression swap is an in-place field update.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContextItem {
    /// A user prompt. **Protected**: it is never submitted to the compression
    /// pipeline and never evicted by the draft pass — its original text stays
    /// verbatim until the LLM compaction folds it into the general summary.
    User { id: u64, original: String },
    /// An assistant text output.
    Assistant {
        id: u64,
        original: String,
        /// Filled in place when the pipeline returns the compressed copy.
        compressed: Option<String>,
        /// False when the output carried a tool call — the spec keeps such
        /// outputs in the structural layer (never prose-compressed).
        compressible: bool,
    },
    /// A tool CALL — structural, never prose-compressed. Renders as an
    /// `assistant` message with native `tool_calls`.
    ToolCall {
        id: u64,
        call_id: String,
        name: String,
        arguments: String,
        /// Gemini 3.x thought signature of the native `functionCall` —
        /// replayed verbatim in the next request's history (the API rejects
        /// the call without it). Empty for inline-JSON calls and for every
        /// other provider.
        thought_signature: String,
    },
    /// A tool RESULT — structural, never prose-compressed. Renders as a
    /// `tool` message with the matching `tool_call_id`.
    ToolResult {
        id: u64,
        call_id: String,
        content: String,
        /// True when the tool produced no useful output (e.g. a zero-match
        /// search). Such a result served its purpose the moment the model
        /// reacted to it, so its chain is dead weight: the automatic sweep in
        /// [`Self::run`] removes it (keeping the newest chain alive) without
        /// waiting for a budget overflow.
        useless: bool,
    },
    /// The final text response of a completed agent loop. Replaces the raw
    /// assistant text in place; protected from the deterministic phases. Also
    /// marks the segment boundary for the pipeline and for the gradual draft
    /// eviction (phase 2 yields here).
    LoopClosure { id: u64, content: String },
    /// The continuation summary produced by the LLM compaction (phase 3, the
    /// last-resort fallback). Replaces the WHOLE timeline in place; renders as
    /// an assistant message. Protected from the deterministic phases — the
    /// next LLM compaction folds it into the new summary (update mode).
    Compaction { id: u64, summary: String },
}

impl ContextItem {
    fn id(&self) -> u64 {
        match self {
            ContextItem::User { id, .. }
            | ContextItem::Assistant { id, .. }
            | ContextItem::ToolCall { id, .. }
            | ContextItem::ToolResult { id, .. }
            | ContextItem::LoopClosure { id, .. }
            | ContextItem::Compaction { id, .. } => *id,
        }
    }

    /// Estimated token cost of what this item currently renders, measured with
    /// the encoding for the active model (`enc` is resolved once by the
    /// manager via [`ContextManager::set_model`], not per item).
    fn tokens(&self, enc: TokenEncoding) -> usize {
        match self {
            ContextItem::User { original, .. } => enc.estimate(original),
            ContextItem::Assistant {
                original,
                compressed,
                ..
            } => enc.estimate(compressed.as_deref().unwrap_or(original)),
            ContextItem::ToolCall {
                name,
                arguments,
                thought_signature,
                ..
            } => enc.estimate(name) + enc.estimate(arguments) + enc.estimate(thought_signature),
            ContextItem::ToolResult { content, .. } => enc.estimate(content),
            ContextItem::LoopClosure { content, .. } => enc.estimate(content),
            ContextItem::Compaction { summary, .. } => enc.estimate(summary),
        }
    }

    fn is_loop(&self) -> bool {
        matches!(self, ContextItem::LoopClosure { .. })
    }

    fn is_tool(&self) -> bool {
        matches!(
            self,
            ContextItem::ToolCall { .. } | ContextItem::ToolResult { .. }
        )
    }

    /// True when this is a compressible assistant text output — the only kind
    /// that may be promoted to a [`ContextItem::LoopClosure`] by
    /// [`ContextManager::close_loop`]. An assistant output that carried a tool
    /// call is structural and can never be a summary of what was done.
    fn is_assistant(&self) -> bool {
        matches!(
            self,
            ContextItem::Assistant {
                compressible: true,
                ..
            }
        )
    }
}

// Deterministic compression (synchronous, same thread as the agent loop)

/// Deterministic compression with batching, so a single pass can never stall
/// the agent loop with an unbounded SVD/MMR pass.
///
/// The pipeline splits the input into sentence-level chunks and runs
/// TF-IDF → LSA (full SVD) → MMR (O(n³)) over them. Without batching, a
/// medium/large turn (a few thousand sentence-chunks) would run ONE pass over
/// the whole text — seconds of work. Splitting into batches of at most
/// [`DETERMINISTIC_MAX_CHUNKS`] sentences bounds every pass to the
/// millisecond range. (Previously batching only kicked in above
/// [`MAX_CONTEXT_TOKENS`] estimated tokens, leaving the medium range — the
/// common case for a single chat turn — unbatched.)
fn compress_text(text: &str) -> String {
    let splitter = TextSplitter::new(CHUNK_CAPACITY);
    let chunks: Vec<&str> = splitter.chunks(text).collect();
    if chunks.len() <= DETERMINISTIC_MAX_CHUNKS {
        // Small text: a single pass over the whole text — identical to the
        // historical single-pass behavior.
        return deterministic_compress(text, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO);
    }
    chunks
        .chunks(DETERMINISTIC_MAX_CHUNKS)
        .map(|batch| {
            // TextSplitter strips boundary whitespace, so rejoin with a single
            // space to avoid gluing the last word of one chunk to the first of
            // the next.
            let batch_text = batch.join(" ");
            deterministic_compress(&batch_text, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO)
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Deterministic compression that can never panic. A panic in the
/// TF-IDF → LSA → MMR pipeline (e.g. a degenerate SVD or a text-splitter edge
/// case) must not crash the agent loop — the draft simply stays raw and the
/// eviction phase remains the fallback.
fn try_compress(text: &str) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compress_text(text))).ok()
}

// LLM compaction (phase 3 — the last-resort fallback, driven by the harness)

/// Serialize one conversation item into the opencode-style transcript the
/// LLM compaction sends to the model.
fn serialize_item(item: &ContextItem) -> String {
    match item {
        ContextItem::User { original, .. } => format!("[User]: {original}"),
        ContextItem::Assistant { original, .. } => format!("[Assistant]: {original}"),
        ContextItem::ToolCall {
            name, arguments, ..
        } => format!("[Assistant tool call]: {name}({arguments})"),
        // Tool results are passed VERBATIM — no truncation (see the note
        // above `SUMMARY_TEMPLATE`): the tools bound their own output, and
        // truncating a payload the model still needs forces it to re-read the
        // source.
        ContextItem::ToolResult { content, .. } => format!("[Tool result]: {content}"),
        ContextItem::LoopClosure { content, .. } => format!("[Assistant]: {content}"),
        // The previous summary stays in the timeline (index 0 after
        // `apply_llm_summary`) — the update-mode instruction references it by
        // this exact label instead of embedding a second copy in the prompt.
        ContextItem::Compaction { summary, .. } => format!("[Previous summary]: {summary}"),
    }
}

/// Build the compaction prompt (opencode-style): create a new anchored summary
/// from the serialized conversation, or update the previous summary when one
/// exists (avoiding the summary-of-summary quality loss).
///
/// The previous summary is NOT embedded here — it stays in the timeline as the
/// FIRST line of the transcript (`[Previous summary]: …`, index 0), and update
/// mode only references it by that label. One copy, never duplicated: the
/// model sees the anchor exactly where the timeline naturally starts, followed
/// by the delta (everything that happened since) to merge into it.
fn build_llm_prompt(has_previous_summary: bool, context: &str) -> String {
    let instruction = if has_previous_summary {
        "Update the summary labeled [Previous summary] at the top of the transcript below, \
         using the rest of the conversation history. Preserve still-true details, \
         remove stale details, and merge in the new facts."
    } else {
        "Create a new anchored summary from the conversation history."
    };
    format!("{instruction}\n\n{SUMMARY_TEMPLATE}\n\n{context}")
}

/// Snapshot of context manager state for bincode persistence. The compression
/// pipeline runs synchronously at the next 80% overflow, so nothing is
/// in-flight between save and restore — the snapshot is a plain clone.
///
/// NOTE: bincode 1.x is positional — ADDING a field is a format break (an
/// old snapshot fails the whole deserialization and the TUI falls back to the
/// JSONL history; the accepted dev-stage tradeoff). `overflow_provider` was
/// added with the context-window overflow recovery, and `split` with the
/// split-and-concatenate contingency.
#[derive(Serialize, Deserialize)]
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub max_tokens: usize,
    /// The provider whose context window overflowed the LLM compaction with
    /// every tool chain drained. While set (same provider), the harness skips
    /// the doomed summarizer call and only re-notifies — until the user
    /// switches the model. `None` = no known stuck overflow.
    pub overflow_provider: Option<String>,
    /// Staging of an in-progress split-and-concatenate, so an interrupted
    /// split resumes exactly where it stopped.
    pub split: Option<SplitState>,
}

/// Snapshot of context manager state for TUI display: the budget percentage
/// and a live token counter (rendered to the left of the budget bar).
#[derive(Default, Clone, Debug)]
pub struct ContextDisplayInfo {
    /// Total estimated tokens held by the context manager.
    pub total_tokens: usize,
    /// Percentage of the budget used (0-100).
    pub budget_pct: u8,
    /// Maximum token budget for the context manager.
    pub max_tokens: usize,
}

/// A compaction-phase notification emitted while [`ContextManager::run`]
/// executes, so the caller (the TUI) can show live feedback in the chat: a
/// stopwatch for the pipeline (phase 1 — the only deterministic phase slow
/// enough to block the agent loop) and one line per draft pass that actually
/// did work. The LLM compaction (phase 3) is driven by the harness, not by
/// `run`, so its start/finish are delivered as a separate
/// [`HarnessEvent::LlmCompaction`](crate::harness::HarnessEvent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionEvent {
    /// Phase 1 (the pipeline) is about to compress drafts — the TUI starts
    /// the stopwatch. Only emitted when there is real work.
    PipelineStarted,
    /// Phase 1 finished compressing — the TUI stops the stopwatch.
    PipelineFinished,
    /// Phase 2 (gradual draft eviction) removed at least one chunk.
    DraftsEvicted,
}

/// Outcome of [`ContextManager::run`]: whether the deterministic phases
/// resolved the overflow, or the harness must run the LLM compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The total dropped below the 80% trigger — nothing else to do.
    Resolved,
    /// Phases 1-2 exhausted every draft and the total is still over the
    /// trigger: the harness must run the LLM compaction (phase 3, the
    /// last-resort fallback, outside the toggle).
    NeedsLlmCompaction,
}

/// The materialization of a pending LLM compaction: the full prompt the
/// harness sends to the model. The manager owns the serialization and the
/// opencode-style template; the harness owns the model call.
pub struct LlmCompactionRequest {
    /// The summarizer's OWN system prompt (defined here in the context
    /// manager, never the agent loop's system prompt from `core.rs`): the
    /// summarizer is a separate agent with its own instructions.
    pub system: String,
    /// The complete summarization prompt (instruction + template + serialized
    /// transcript of the entire remaining context).
    pub prompt: String,
}

/// The SYSTEM prompt of the split summarizer — the dedicated agent that
/// processes the timeline in sequential chunks for the split-and-concatenate
/// contingency. Unlike [`SUMMARIZER_SYSTEM`] (one-shot over the whole
/// timeline), this one must keep the final buffer continuous: every
/// continuation chunk is glued onto the tail of the previous summary.
const SPLIT_SUMMARIZER_SYSTEM: &str = "\
You are a conversation summarizer for an AI coding agent.
You summarize a long conversation in SEQUENTIAL CHUNKS: each chunk continues
where the previous one ended, and your summaries are concatenated into ONE
final anchored summary.
Rules:
- First chunk: build the anchored summary following the template exactly.
- Continuation chunks: extend the summary naturally from the tail of the
  previous summary shown in [Continuation context] — keep its structure and
  style, add the new facts, never restate what is already covered. The final
  result must read as ONE continuous document with no visible seam.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers,
  symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures
  and error messages exactly as they appear (inside fenced blocks). Never
  paraphrase code.
- Keep facts precise; do not invent details that are not in the chunk.
- When a token limit is given, respect it strictly — prefer terse bullets.
- Do not mention the summary process or that context was compacted.";

/// Build the split-and-concatenate prompt for one chunk.
///
/// The FIRST chunk carries the full [`SUMMARY_TEMPLATE`] so it produces the
/// anchored skeleton; every continuation chunk gets the tail of the previous
/// summary as a continuity snippet and is told to extend it — never restart
/// the template — so the concatenated buffer reads seamlessly.
///
/// The token-limit line is only included when the buffer accumulation becomes
/// concerning ([`SPLIT_WARN_PCT`] of the ceiling): before that the model is
/// left to act naturally, per the "observe and only warn when it matters"
/// design.
fn build_split_prompt(
    first_chunk: bool,
    continuity: &str,
    chunk: &str,
    should_warn: bool,
    target_tokens: Option<usize>,
    remaining_tokens: usize,
    buffer_tokens: usize,
) -> String {
    let mut out = String::new();
    if first_chunk {
        out.push_str(SUMMARY_TEMPLATE);
        out.push_str("\n\n");
    } else {
        out.push_str(
            "Continue the anchored summary you started in the previous call.\n\
             Preserve its section structure and style; extend the existing sections with the new \
             facts. Do NOT restart the template from scratch — the final summary must read as ONE \
             continuous document.\n\n\
             [Continuation context — the tail of your previous summary]:\n",
        );
        out.push_str(continuity);
        out.push('\n');
    }
    if should_warn && let Some(target) = target_tokens {
        out.push_str(&format!(
            "\nTOKEN LIMIT: {buffer_tokens} tokens are already summarized and the final \
             summary must stay within budget. Compress the chunk below to AT MOST ~{target} \
             tokens — terse bullets, only the essential facts, paths, hashline anchors and code.\n"
        ));
    }
    out.push_str(&format!(
        "\n~{remaining_tokens} tokens of conversation remain to summarize.\n\n"
    ));
    out.push_str("[Conversation chunk to summarize]:\n");
    out.push_str(chunk);
    out
}

/// Persistent staging state of the split-and-concatenate contingency.
///
/// The timeline is NEVER touched while a split is in progress: chunks are
/// serialized from the items and their summaries accumulate here. Only when
/// every item has been consumed does [`ContextManager::commit_split`] replace
/// the timeline with the concatenated buffer — atomically. Because the
/// timeline is untouched, this state can be persisted with the snapshot and
/// the split resumes exactly where it stopped.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SplitState {
    /// Concatenated chunk summaries produced so far (the growing buffer).
    pub buffer: String,
    /// Id of the next item to summarize (`None` = the beginning of the
    /// timeline). After a chunk is sent and succeeds, this advances past the
    /// chunk's last item.
    pub cursor: Option<u64>,
    /// Tail of the last chunk summary, carried into the next chunk so the
    /// final buffer reads as one continuous summary.
    pub continuity: String,
    /// The model window the split is sized against (chunk budget + ceiling).
    pub window: usize,
    /// Estimated tokens currently held by the buffer (persisted so the
    /// projection does not depend on re-estimation after a restart).
    pub buffer_tokens: usize,
}

/// Live budget projection of an in-progress split, used to decide when the
/// summarizer must be asked to be more terse.
#[derive(Debug, Clone, Copy)]
pub struct SplitProjection {
    /// Tokens currently accumulated in the buffer.
    pub buffer_tokens: usize,
    /// The ceiling: [`SPLIT_BUFFER_MAX_PCT`] of the model window.
    pub ceiling: usize,
    /// The warning point: [`SPLIT_WARN_PCT`] of the ceiling.
    pub warn_at: usize,
    /// Whether the limit message should be sent on the next chunk.
    pub should_warn: bool,
    /// Estimated tokens of the items not yet summarized.
    pub remaining_tokens: usize,
}

/// One chunk request of the split-and-concatenate contingency: the prompt the
/// harness streams to the summarizer, plus the id of the chunk's last item
/// (used by [`ContextManager::advance_split`] to move the cursor on success).
#[derive(Debug)]
pub struct SplitChunkRequest {
    /// The split summarizer's OWN system prompt.
    pub system: String,
    /// The complete chunk prompt (instructions + template/continuity +
    /// metadata + serialized chunk).
    pub prompt: String,
    /// Id of the LAST item covered by this chunk; the cursor advances past it
    /// when the chunk succeeds.
    pub chunk_end: Option<u64>,
}

/// Orchestrates the synchronous TF-IDF → LSA → MMR compression of the whole
/// conversation, the in-place swaps, the 80% pipeline-first funnel (pipeline
/// → gradual draft eviction), the useless tool-chain sweep, and the
/// `LoopClosure` promotion of each finished agent loop. The LLM compaction
/// (phase 3) is requested via [`RunOutcome::NeedsLlmCompaction`] and applied
/// by the harness through [`Self::apply_llm_summary`].
pub struct ContextManager {
    /// Conversation items in display order (oldest first). One item = one
    /// message; positions are stable.
    items: VecDeque<ContextItem>,
    /// Persistent position of the gradual draft eviction (phase 2): the id of
    /// the NEXT item to consider. `None` means the walk starts at the
    /// beginning of the timeline. The eviction removes one whole chunk per
    /// stop and parks this cursor, so the next budget overflow resumes exactly
    /// where this one stopped. Scheduling bias only — not persisted; a
    /// restored manager simply starts over.
    draft_cursor: Option<u64>,
    /// Segment frontier: the id of the FIRST item of the current segment —
    /// the segment phases 1 and 2 are working on. `None` means the frontier
    /// is at the start of the timeline. Phase 1 compresses the segment, phase
    /// 2 evicts it, and when phase 2 exhausts the segment's drafts (reaching
    /// its closing [`LoopClosure`]) the frontier advances past the closure —
    /// the funnel then repeats (tudo se repete) so the pipeline takes the
    /// next segment in the same `run()`. Phase 2 can never cross the frontier
    /// into a segment the pipeline has not processed, so
    /// eviction structurally never touches unsummarized prose. Scheduling
    /// bias only — not persisted; a restored manager starts over.
    segment_start: Option<u64>,
    /// Live compaction notifications for the TUI: called synchronously while
    /// [`Self::run`] executes so the caller can show the pipeline stopwatch
    /// and one line per phase that actually did work. Scheduling-only — not
    /// part of the snapshot; the harness wires it per agent loop via
    /// [`Self::set_compaction_observer`]. `Send` so a [`Harness`] holding the
    /// manager stays movable into a tokio task.
    compaction_observer: Option<Box<dyn FnMut(CompactionEvent) + Send>>,
    /// Monotonic id counter for items/jobs.
    next_id: u64,
    /// Token budget before the 80% compaction trigger.
    max_tokens: usize,
    /// tiktoken encoding matching the active model (set by the harness via
    /// [`Self::set_model`]). Defaults to [`TokenEncoding::Cl100k`] — the
    /// generic cross-provider estimate.
    encoding: TokenEncoding,
    /// The provider whose context window overflowed the LLM compaction with
    /// every tool chain drained (see [`Self::overflow_stuck`]). Persisted with
    /// the snapshot so a stuck session stays notified across turns until the
    /// user switches the model.
    overflow_provider: Option<String>,
    /// Persistent staging of the split-and-concatenate contingency (the
    /// known-window model-switch path). The timeline is untouched while it is
    /// `Some`; the buffer is committed atomically by [`Self::commit_split`].
    /// Persisted with the snapshot so an interrupted split resumes exactly
    /// where it stopped.
    split: Option<SplitState>,
    /// The dedicated protected TODO block: mirrors the tools' `Plan` list and
    /// renders it as a protected `user` message at the FRONT of the messages.
    /// Never compressed, evicted, drained or summarized — see [`todo_ctxt`]
    /// for the protection and removal rules.
    todo: TodoContext,
}

/// Build a plain assistant text message (no tool calls).
fn assistant_message(text: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            items: VecDeque::new(),
            draft_cursor: None,
            segment_start: None,
            compaction_observer: None,
            next_id: 1,
            max_tokens,
            encoding: TokenEncoding::Cl100k,
            overflow_provider: None,
            split: None,
            todo: TodoContext::new(),
        }
    }

    /// Create a new ContextManager with automatic context window discovery.
    ///
    /// Attempts to discover the model's context window via public APIs (OpenRouter
    /// and Anthropic). Falls back to [`MAX_CONTEXT_TOKENS`] if discovery fails.
    ///
    /// # Arguments
    ///
    /// * `model_name` - The model identifier (e.g., "gpt-4o", "claude-sonnet-4-5")
    ///
    /// # Returns
    ///
    /// A new ContextManager with the discovered context window, or the default
    /// [`MAX_CONTEXT_TOKENS`] if discovery fails.
    pub async fn with_discovered_context(model_name: &str) -> Self {
        // On-disk catalog caching under `~/.local/share/cosh/cache`, so repeated
        // launches reuse the downloaded models.dev / OpenRouter catalogs.
        let max_tokens = discover_context_window(model_name, Some("cosh/cache"))
            .await
            .unwrap_or(MAX_CONTEXT_TOKENS);

        Self::new(max_tokens)
    }

    /// Route compaction-phase notifications to `f`, called synchronously
    /// while [`Self::run`] executes (the TUI uses this to show the pipeline
    /// stopwatch and the per-phase lines live in the chat). Replaces any
    /// previous observer.
    pub fn set_compaction_observer(&mut self, f: impl FnMut(CompactionEvent) + Send + 'static) {
        self.compaction_observer = Some(Box::new(f));
    }

    fn emit(&mut self, event: CompactionEvent) {
        if let Some(f) = self.compaction_observer.as_mut() {
            f(event);
        }
    }

    /// Set the active model so token counts are estimated with the closest
    /// tiktoken encoding (`None` → the default [`TokenEncoding::Cl100k`]). The
    /// harness calls this whenever the connector/model changes (loop start,
    /// fallback switch).
    pub fn set_model(&mut self, model: Option<&str>) {
        self.encoding = TokenEncoding::for_model(model);
    }

    /// Override the token budget, e.g. from context-window discovery for the
    /// active model ([`Self::with_discovered_context`]). The 80% compaction
    /// trigger ([`Self::run`]) re-scales with it, so the LLM compaction fires
    /// at a sane fraction of the model's REAL window instead of a hardcoded
    /// default that is far below it.
    pub fn set_max_tokens(&mut self, max_tokens: usize) {
        self.max_tokens = max_tokens;
    }

    /// Replace the mirrored tool TODO list. The harness syncs it from the
    /// tools' `Plan` state at loop start and after every `plan_*` dispatch.
    /// The dedicated [`TodoContext`] renders it as a protected block at the
    /// FRONT of the messages — structurally immune to every compaction phase
    /// (see [`todo_ctxt`] for the removal rules: all tasks terminal, or the
    /// model empties the plan).
    pub fn set_todo_list(&mut self, list: TodoList) {
        self.todo.sync(list);
    }

    // Ingestion

    /// Add a user prompt. Protected by construction: it is never compressed
    /// and never evicted — its original text stays verbatim until the LLM
    /// compaction (phase 3) folds it into the general summary.
    pub fn add_user(&mut self, text: &str) {
        let id = self.next_id();
        self.items.push_back(ContextItem::User {
            id,
            original: text.to_string(),
        });
    }

    /// Add an assistant text output. When `compressible` is true the raw text
    /// is rendered in place until the pipeline (phase 1 of the compaction)
    /// summarizes it — synchronously, at the next 80% overflow. When false
    /// (the output carried a tool call) it stays structural and is never
    /// submitted to the compressor; either kind is a removable draft for the
    /// eviction pass.
    ///
    /// The FINAL output of a loop needs no special casing here: the harness
    /// calls [`Self::close_loop`] the moment the loop ends, promoting the
    /// ORIGINAL text verbatim into a `LoopClosure` — even if a pipeline pass
    /// had already summarized the raw draft, the delivered final answer is
    /// always the original text. A turn the loop provably advances past is
    /// fair game for the pipeline.
    pub fn add_assistant(&mut self, text: &str, compressible: bool) {
        let id = self.next_id();
        self.items.push_back(ContextItem::Assistant {
            id,
            original: text.to_string(),
            compressed: None,
            compressible,
        });
    }

    /// Add a tool CALL. Structural, never prose-compressed.
    pub fn add_tool_call(&mut self, call_id: &str, name: &str, arguments: &str) {
        self.add_tool_call_with_signature(call_id, name, arguments, "");
    }

    /// Add a tool CALL carrying a Gemini 3.x `thought_signature` (the native
    /// `functionCall` sibling the model attached — must be replayed verbatim
    /// in the next request's history). Structural, never prose-compressed.
    /// Every non-Gemini path passes an empty signature.
    pub fn add_tool_call_with_signature(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        thought_signature: &str,
    ) {
        let id = self.next_id();
        self.items.push_back(ContextItem::ToolCall {
            id,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: thought_signature.to_string(),
        });
    }

    /// Add a tool RESULT. Structural, never prose-compressed.
    pub fn add_tool_result(&mut self, call_id: &str, content: &str) {
        self.add_tool_result_flagged(call_id, content, false);
    }

    /// Add a tool RESULT with an explicit `useless` marker. Useless chains are
    /// dead weight cleaned automatically by the sweep in [`Self::run`] (see
    /// the `useless` field); everything else behaves exactly like
    /// [`Self::add_tool_result`].
    pub fn add_tool_result_flagged(&mut self, call_id: &str, content: &str, useless: bool) {
        let id = self.next_id();
        self.items.push_back(ContextItem::ToolResult {
            id,
            call_id: call_id.to_string(),
            content: content.to_string(),
            useless,
        });
    }

    /// True when the last item is a user prompt whose original text equals
    /// `text` (used by the harness to avoid duplicating the current input when
    /// it was already loaded via `with_history`).
    pub fn last_user_equals(&self, text: &str) -> bool {
        matches!(
            self.items.back(),
            Some(ContextItem::User { original, .. }) if original == text
        )
    }

    /// Drop ABANDONED input turns: when the timeline ends with two or more
    /// consecutive `User` turns with NO output between them (assistant text,
    /// tool work, LoopClosure or summary), every turn but the newest is an
    /// input the user gave up on — the run ended without the LLM producing
    /// anything (typically cancelled with Esc), and a new input followed.
    /// Each older turn of the run is removed, so the model only ever sees the
    /// newest input. A turn that produced ANY output after it is never
    /// touched.
    ///
    /// Called by the harness at loop start right after the fresh input is
    /// added, so an abandoned prompt never survives into the request.
    ///
    /// Returns whether at least one abandoned turn was removed.
    pub fn remove_abandoned_inputs(&mut self) -> bool {
        // Count the trailing consecutive User turns.
        let mut trailing_users = 0usize;
        for item in self.items.iter().rev() {
            if matches!(item, ContextItem::User { .. }) {
                trailing_users += 1;
            } else {
                break;
            }
        }
        if trailing_users <= 1 {
            return false;
        }
        // Keep the NEWEST input (the last item); drop the rest of the run.
        let start = self.items.len() - trailing_users;
        let end = self.items.len() - 1;
        self.items.drain(start..end);
        // Scheduling bias only — the removed tail may hold the draft cursor
        // and the segment frontier; reset them so the next overflow starts
        // fresh.
        self.draft_cursor = None;
        self.segment_start = None;
        true
    }

    /// Promote the last assistant text output of the finished agent loop into a
    /// [`ContextItem::LoopClosure`], replacing the raw text in place. Guard: if
    /// the last item is not an assistant text (e.g. it was a tool call), it
    /// cannot be a summary of what was done, so no `LoopClosure` is created.
    ///
    /// The harness has now told us this turn was the loop's FINAL output: the
    /// ORIGINAL text is promoted verbatim, so the final answer is never
    /// compressed.
    pub fn close_loop(&mut self) {
        let Some(last) = self.items.back() else {
            return;
        };
        if !last.is_assistant() {
            return;
        }
        let ContextItem::Assistant { id, original, .. } = self.items.pop_back().unwrap() else {
            return;
        };
        self.items.push_back(ContextItem::LoopClosure {
            id,
            content: original,
        });
    }

    // Compaction

    /// Per-iteration tick: run the 80% compaction when the held context
    /// reaches the trigger.
    ///
    /// The funnel is **pipeline-first by construction** — phase 1 (the
    /// pipeline) ALWAYS leads, and phase 2 (gradual draft eviction) only runs
    /// when the pipeline could not bring the total below the 80% trigger:
    ///
    ///   1. **The pipeline** — compresses the CURRENT segment's compressible
    ///      assistant drafts synchronously on the agent loop's thread; at the
    ///      segment's closing [`LoopClosure`], below the trigger → done, still
    ///      over → the pass STOPS here (parada por segmento) and phase 2
    ///      takes over the same segment;
    ///   2. **Draft eviction** — one whole assistant draft at a time from a
    ///      persistent cursor, stopping the moment the total drops below 80%.
    ///      When the segment's drafts are exhausted (the pass reaches the
    ///      closing [`LoopClosure`]), the **segment frontier** advances past
    ///      the closure and the funnel repeats inside the same call — the
    ///      pipeline takes the next segment (tudo se repete).
    ///
    /// Because the pipeline always runs FIRST and phase 2 is bounded by the
    /// segment frontier, eviction structurally never touches prose the
    /// pipeline could still summarize. This invariant comes from the ORDER,
    /// not from an eligibility rule: if the pipeline breaks or times out on a
    /// draft, that draft stays raw but remains evictable, so the system
    /// degrades instead of deadlocking.
    ///
    /// User prompts, `LoopClosure`s, tool chains and compaction summaries are
    /// protected from both phases. When the alternation exhausts every draft
    /// and the total is still over the trigger, the manager returns
    /// [`RunOutcome::NeedsLlmCompaction`] — the harness then runs the LLM
    /// compaction (phase 3, the last-resort fallback, outside the toggle) and
    /// applies the summary via [`Self::apply_llm_summary`].
    ///
    /// Before anything else, the **useless tool-chain sweep** runs (outside
    /// the toggle, regardless of the budget): chains whose result is marked
    /// `useless` are dropped automatically, keeping the newest chain alive.
    ///
    /// Phases 1-2 aim for the 80% trigger (minimum decompaction): a pass ends
    /// just under the ceiling, so in a busy tool loop the next `run()` — called
    /// after every dispatch — re-triggers with a minimal removal each time.
    /// That is the intended rolling eviction, not a bug.
    pub fn run(&mut self) -> RunOutcome {
        // Pre-phase (outside the toggle): drop useless tool chains — dead
        // weight cleaned automatically, even under the budget.
        self.sweep_useless_chains();
        if self.total_tokens() < self.trigger() {
            return RunOutcome::Resolved;
        }
        // Segment-by-segment grind ("tudo se repete"): phase 1 ALWAYS leads
        // for the current segment; phase 2 drains it when the pipeline could
        // not resolve. When phase 2 exhausts the segment (the frontier
        // advances past its closing LoopClosure) and the total is still over
        // the trigger, the cycle REPEATS inside this same call — the
        // pipeline takes the next segment immediately — until the total
        // drops below the trigger or every draft is gone. Each iteration
        // advances the frontier past at least one segment (or reaches the
        // end of the timeline, where removing every draft implies
        // `drafts_exhausted`), so the loop terminates.
        loop {
            // Phase 1 ALWAYS leads. It compresses the CURRENT segment's
            // drafts and stops at the segment's closing LoopClosure (parada
            // por segmento); resolving here is the minimum decompaction.
            if self.pipeline_pass() {
                return RunOutcome::Resolved;
            }
            // Only when the pipeline could not bring the total below the
            // trigger does phase 2 run — bounded by the segment frontier, so
            // everything it can remove has already been through the pipeline
            // (or is structural / failed to compress). Eviction never removes
            // prose the pipeline could still summarize: the invariant is
            // guaranteed by this ORDER, not by an eligibility rule, so a
            // broken or timed-out pipeline degrades gracefully (the raw
            // draft remains evictable) instead of deadlocking the context.
            if self.evict_drafts() {
                return RunOutcome::Resolved;
            }
            // Retention (context-window-aware): before the LLM compaction,
            // trim the CONTENT of old tool results in place — the tool-chain
            // contract stays intact and the long-session memory growth is
            // bounded even when no draft can be compressed or evicted.
            if self.trim_stale_tool_results() {
                return RunOutcome::Resolved;
            }
            // Both phases ran and the total is still over the trigger. The
            // deterministic grind is only exhausted when no draft remains —
            // then the LLM compaction is the only way down (phase 3, last
            // resort, outside the toggle). Otherwise phase 2 exhausted the
            // current segment (the frontier advanced) — loop and let the
            // pipeline take the next segment.
            if self.drafts_exhausted() {
                return RunOutcome::NeedsLlmCompaction;
            }
        }
    }

    /// Phase 1 of the compaction: the compression pipeline, now synchronous.
    ///
    /// Compresses every compressible assistant draft of the CURRENT segment —
    /// the drafts from the segment frontier up to the next
    /// [`ContextItem::LoopClosure`]. At the segment boundary the budget is
    /// checked: below the 80% trigger → the pass is done; still over → the
    /// pass STOPS here (parada por segmento) and phase 2 takes over the same
    /// segment — the next segment's drafts wait until the current one is
    /// exhausted. A segment's drafts are compressed as a coherent block, so
    /// the model sees a consistent (compressed) view of each completed loop
    /// instead of a mixed raw/summary mix.
    ///
    /// User prompts are never candidates (they are protected) — only
    /// compressible assistant texts.
    ///
    /// Returns `true` when the total dropped below the trigger (the funnel
    /// stops here); `false` when the segment is fully compressed and the
    /// total is still over the trigger — the funnel falls through to the
    /// draft pass for the same segment.
    ///
    /// Runs on the agent loop's thread — there is no worker thread anymore.
    fn pipeline_pass(&mut self) -> bool {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            return true;
        }
        // Walk from the segment frontier (the current segment's start).
        let mut idx = self.segment_start_idx();
        // Only report the pass when the CURRENT segment holds at least one
        // compressible draft — an empty pass is instant and invisible to the
        // user, so it must not surface a stopwatch line.
        let has_work = self
            .items
            .iter()
            .skip(idx)
            .take_while(|it| !it.is_loop())
            .any(|it| {
                matches!(
                    it,
                    ContextItem::Assistant {
                        compressible: true,
                        compressed: None,
                        ..
                    }
                )
            });
        if has_work {
            self.emit(CompactionEvent::PipelineStarted);
        }
        // No compressible draft in the current segment: the walk below could
        // not change the total, and `run` only calls this over the trigger —
        // skip the O(n) walk entirely (pipeline-first runs this phase on every
        // overflow).
        if !has_work {
            return false;
        }
        while idx < self.items.len() {
            if self.items[idx].is_loop() {
                // Segment boundary (parada por segmento): the segment's drafts
                // are fully compressed. Below the trigger → done; still over →
                // STOP here and hand the SAME segment to phase 2 — the next
                // segment's drafts wait until the current one is exhausted.
                if has_work {
                    self.emit(CompactionEvent::PipelineFinished);
                }
                return total < trigger;
            }
            let eligible = matches!(
                &self.items[idx],
                ContextItem::Assistant {
                    compressible: true,
                    compressed: None,
                    ..
                }
            );
            if !eligible {
                idx += 1;
                continue;
            }
            // Snapshot the job first (releases the borrow before mutating).
            let original = match &self.items[idx] {
                ContextItem::Assistant { original, .. } => original.clone(),
                _ => unreachable!(),
            };
            let Some(compressed) = try_compress(&original) else {
                // Degrade: keep the draft raw — the eviction pass still
                // applies.
                idx += 1;
                continue;
            };
            let old_tokens = self.items[idx].tokens(self.encoding);
            match &mut self.items[idx] {
                ContextItem::Assistant { compressed: c, .. } => *c = Some(compressed),
                _ => unreachable!(),
            }
            total = total
                .saturating_sub(old_tokens)
                .saturating_add(self.items[idx].tokens(self.encoding));
            idx += 1;
        }
        // End of the timeline — still over the trigger → not resolved.
        if has_work {
            self.emit(CompactionEvent::PipelineFinished);
        }
        total < trigger
    }

    /// True when the item `id` belongs to a tool chain whose RESULT is marked
    /// `useless` — a call that produced no useful output, so its chain is dead
    /// weight once the model has reacted to it.
    fn chain_is_useless(&self, id: u64) -> bool {
        let Some(idx) = self.items.iter().position(|it| it.id() == id) else {
            return false;
        };
        let call_id = match &self.items[idx] {
            ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. } => {
                call_id
            }
            _ => return false,
        };
        self.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::ToolResult {
                    call_id: c,
                    useless: true,
                    ..
                } if c == call_id
            )
        })
    }

    /// Automatic dead-weight cleanup, OUTSIDE the toggle: remove every tool
    /// chain whose result is marked `useless` (e.g. a zero-match
    /// `find_grep`/`find_glob`), **except the newest chain** — the one the
    /// model has not seen yet. Runs at the start of every [`Self::run`]
    /// regardless of the budget, so useless chains are cleaned as soon as the
    /// model has had one read of them instead of waiting for a budget
    /// overflow.
    fn sweep_useless_chains(&mut self) {
        // The newest chain (the one the model has not seen yet) is preserved:
        // its CALL id is the chain identity, so BOTH halves are excluded.
        let newest_call_id: Option<String> =
            self.items
                .iter()
                .rev()
                .find(|it| it.is_tool())
                .and_then(|it| match it {
                    ContextItem::ToolCall { call_id, .. }
                    | ContextItem::ToolResult { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                });
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter(|it| it.is_tool())
            .filter(|it| match it {
                ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. } => {
                    newest_call_id.as_deref() != Some(call_id.as_str())
                }
                _ => true,
            })
            .filter(|it| self.chain_is_useless(it.id()))
            .map(ContextItem::id)
            .collect();
        for id in ids {
            if let Some(idx) = self.items.iter().position(|it| it.id() == id) {
                self.remove_item(idx);
            }
        }
    }

    /// Retention under budget pressure: trim the CONTENT of old tool results
    /// to a short preview instead of holding every full tool output for the
    /// rest of the session (the growth that drives long-session OOMs).
    ///
    /// Contract-safe by construction: the item stays a `tool` message with its
    /// `call_id` — only the content is shortened, so the native
    /// `tool_call → tool` pairing the providers require never breaks. The
    /// NEWEST chain is left intact (the model has not read that result yet);
    /// older results are trimmed in one pass.
    ///
    /// Idempotent: an already-trimmed result is skipped, so a later overflow
    /// costs one cheap scan and never re-mangles the text.
    ///
    /// Returns `true` when the trim brought the total below the 80% trigger.
    fn trim_stale_tool_results(&mut self) -> bool {
        if self.total_tokens() < self.trigger() {
            return true;
        }
        // The newest chain (the one the model has not seen yet) is preserved:
        // its CALL id is the chain identity, so its result stays full.
        let newest_call_id: Option<String> =
            self.items
                .iter()
                .rev()
                .find(|it| it.is_tool())
                .and_then(|it| match it {
                    ContextItem::ToolCall { call_id, .. }
                    | ContextItem::ToolResult { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                });
        let mut trimmed = false;
        for it in self.items.iter_mut() {
            if let ContextItem::ToolResult {
                call_id,
                content,
                ..
            } = it
            {
                if newest_call_id.as_deref() == Some(call_id.as_str()) {
                    continue;
                }
                if content.starts_with(TOOL_RESULT_TRIM_MARKER) {
                    continue;
                }
                if content.len() <= TOOL_RESULT_PREVIEW_CHARS {
                    continue;
                }
                let prefix: String = content
                    .chars()
                    .take(TOOL_RESULT_PREVIEW_CHARS)
                    .collect();
                let saved = content.len() - prefix.len();
                *content = format!("{TOOL_RESULT_TRIM_MARKER}{saved} chars] {prefix}");
                trimmed = true;
            }
        }
        trimmed && self.total_tokens() < self.trigger()
    }

    /// Phase 2 of the compaction: gradual draft eviction.
    ///
    /// Removes ONE whole chunk — an assistant draft — at a time, walking the
    /// CURRENT segment oldest-first from a persistent cursor. After each
    /// removal the budget is checked: below the 80% trigger → stop (the
    /// cursor parks on the next item, so the NEXT overflow resumes exactly
    /// here — the decoupling is spread over many overflows and the context
    /// lives much longer).
    ///
    /// `run` only calls this AFTER the pipeline finished the current segment,
    /// so every draft seen here has already been through it (or is structural
    /// — `compressible: false` — or failed to compress): eviction never
    /// removes prose the pipeline could still summarize. That is guaranteed by
    /// the calling ORDER and the segment frontier, not by an eligibility rule
    /// — a broken pipeline degrades gracefully (the raw draft remains
    /// evictable) instead of deadlocking.
    ///
    /// User prompts, `LoopClosure`s, tool chains and compaction summaries are
    /// never removed here — user prompts are protected and the other three are
    /// structural segments, not drafts.
    ///
    /// Returns `true` when the total dropped below the trigger (the cursor is
    /// parked for the next overflow). Returns `false` when the segment's
    /// drafts are exhausted — the pass reached the segment's closing
    /// [`ContextItem::LoopClosure`] and the segment frontier advanced past it
    /// (the funnel then repeats, so the pipeline takes the next segment in
    /// the same `run()` — tudo se repete) — or the end of the timeline still
    /// over the trigger (nothing left to evict anywhere). Ending the timeline
    /// below the trigger resets the cursor and the frontier so the next
    /// overflow starts fresh.
    ///
    /// Cursor parking: the walk-yield at a closure parks PAST it (`idx + 1`,
    /// the next segment starts there), while a below-the-trigger stop with a
    /// closure as the next item parks ON the closure — both are safe because
    /// the resume looks for the first item with `id >= cursor`.
    fn evict_drafts(&mut self) -> bool {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            // Defensive — `run` only calls this over the trigger.
            return true;
        }
        // Resume from the persistent cursor (the id of the next item to
        // consider), floored at the segment frontier so the pass can never
        // cross into a segment the pipeline has not processed. Ids are
        // monotonic, so the first item with `id >= cursor` is the resume
        // point even if the cursor item itself was removed by another phase.
        let seg_idx = self.segment_start_idx();
        let mut idx = match self.draft_cursor {
            Some(cid) => self
                .items
                .iter()
                .position(|it| it.id() >= cid)
                .unwrap_or(self.items.len())
                .max(seg_idx),
            None => seg_idx,
        };
        let mut removed_any = false;
        loop {
            // Advance past the non-removable items. A LoopClosure is the
            // segment boundary: the segment's drafts are exhausted — the
            // frontier advances past it and the next overflow's pipeline
            // takes the next segment.
            while idx < self.items.len() {
                if self.items[idx].is_loop() {
                    self.draft_cursor = self.items.get(idx + 1).map(|it| it.id());
                    self.segment_start = self.items.get(idx + 1).map(|it| it.id());
                    if removed_any {
                        self.emit(CompactionEvent::DraftsEvicted);
                    }
                    return total < trigger;
                }
                if !self.is_removable_draft(idx) {
                    idx += 1;
                    continue;
                }
                break;
            }
            if idx >= self.items.len() {
                // End of the timeline — the LAST segment is exhausted (or
                // never had drafts). Below the trigger → resolved, and both
                // the cursor and the frontier reset so the next overflow
                // starts fresh. Still over → nothing left to evict anywhere:
                // the deterministic grind is exhausted.
                self.draft_cursor = None;
                self.segment_start = None;
                if removed_any {
                    self.emit(CompactionEvent::DraftsEvicted);
                }
                return total < trigger;
            }
            // Remove ONE chunk and check the budget immediately.
            let removed = self.remove_item(idx);
            total = total.saturating_sub(removed);
            removed_any = true;
            // The next item shifted into `idx`; park the cursor there.
            self.draft_cursor = self.items.get(idx).map(|it| it.id());
            if total < trigger {
                // Below the trigger — resolved (whether or not the very next
                // item is a LoopClosure; either way the cursor is parked on
                // it, so the next overflow resumes from there).
                if removed_any {
                    self.emit(CompactionEvent::DraftsEvicted);
                }
                return true;
            }
        }
    }

    /// True when the item at `idx` is a removable draft chunk: an assistant
    /// output. User prompts are protected (never removed), `LoopClosure`s,
    /// tool chains and compaction summaries are structural segments, not
    /// drafts.
    fn is_removable_draft(&self, idx: usize) -> bool {
        matches!(self.items[idx], ContextItem::Assistant { .. })
    }

    /// True when neither deterministic phase has any work left: no
    /// compressible draft for the pipeline and no removable draft for the
    /// eviction pass. Since user prompts, `LoopClosure`s, tool chains and
    /// compaction summaries are protected from both phases, "no `Assistant`
    /// items" is exactly "the grind is exhausted".
    fn drafts_exhausted(&self) -> bool {
        !self
            .items
            .iter()
            .any(|it| matches!(it, ContextItem::Assistant { .. }))
    }

    /// Build the LLM-compaction request (phase 3, the last-resort fallback):
    /// the entire remaining context — protected items included — serialized
    /// into an opencode-style transcript, wrapped in the summarization prompt.
    ///
    /// A [`ContextItem::Compaction`] item (the previous summary) switches the
    /// prompt to update mode: the summary is NOT embedded in the instruction —
    /// it stays in the timeline as the first line of the transcript
    /// (`[Previous summary]: …`), and the instruction references it by that
    /// label so the model merges the new facts into the anchor instead of
    /// re-summarizing from scratch (avoiding summary-of-summary quality loss).
    ///
    /// Returns `None` when there is nothing to compact (empty timeline, or
    /// the total is already below the trigger — defensive, the harness only
    /// calls this after [`RunOutcome::NeedsLlmCompaction`]).
    pub fn llm_compaction_request(&self) -> Option<LlmCompactionRequest> {
        if self.items.is_empty() || self.total_tokens() < self.trigger() {
            return None;
        }
        let has_previous_summary = self
            .items
            .iter()
            .any(|it| matches!(it, ContextItem::Compaction { .. }));
        let context = self
            .items
            .iter()
            .map(serialize_item)
            .collect::<Vec<String>>()
            .join("\n\n");
        Some(LlmCompactionRequest {
            system: SUMMARIZER_SYSTEM.to_string(),
            prompt: build_llm_prompt(has_previous_summary, &context),
        })
    }

    /// Apply the LLM-compaction summary: replace the ENTIRE timeline with a
    /// single [`ContextItem::Compaction`] (the continuation summary) and reset
    /// the scheduling state (cursor + toggle). Returns whether the total is
    /// now below the 80% trigger — the summary should be small enough, but a
    /// degenerate huge summary is surfaced so the harness can report failure.
    pub fn apply_llm_summary(&mut self, summary: String) -> bool {
        let id = self.next_id();
        self.items.clear();
        self.items
            .push_back(ContextItem::Compaction { id, summary });
        self.draft_cursor = None;
        self.segment_start = None;
        // A successful compaction is proof the provider accepts the context
        // again — any recorded stuck overflow is no longer relevant.
        self.clear_overflow();
        self.total_tokens() < self.trigger()
    }

    // Split-and-concatenate (the known-window contingency, driven by the
    // harness)
    //
    // When the ACTIVE model's window is KNOWN and the held context exceeds it
    // (a model switch to a smaller window, or a provider error that reports
    // the window), the timeline is summarized in sequential chunks and the
    // summaries are concatenated into ONE staging buffer. The timeline is
    // NEVER touched during the process — the final commit replaces it with
    // the buffer atomically, exactly like a normal LLM compaction. This is a
    // contingency path only: the normal 80% funnel and the single-shot LLM
    // compaction are untouched.

    /// The current token budget — the discovered window or the default. The
    /// harness uses it (with the error-reported window) to detect the
    /// known-window, context-exceeds-window split trigger.
    #[must_use]
    pub const fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// True when a split-and-concatenate is in progress (or staged after a
    /// restore, waiting to resume).
    #[must_use]
    pub fn split_active(&self) -> bool {
        self.split.is_some()
    }

    /// Start (or re-size) the split-and-concatenate contingency for `window`.
    /// Starting again while a split is already staged only updates the window
    /// — the buffer and the cursor are preserved (resume semantics).
    pub fn begin_split(&mut self, window: usize) {
        match self.split.as_mut() {
            Some(split) => split.window = window,
            None => {
                self.split = Some(SplitState {
                    buffer: String::new(),
                    cursor: None,
                    continuity: String::new(),
                    window,
                    buffer_tokens: 0,
                });
            }
        }
    }

    /// Live budget projection of the in-progress split: buffer accumulation
    /// vs the ceiling, plus the tokens still to summarize.
    #[must_use]
    pub fn split_projection(&self) -> SplitProjection {
        let split = self.split.as_ref();
        let window = split.map_or(self.max_tokens, |s| s.window);
        let buffer_tokens = split.map_or(0, |s| s.buffer_tokens);
        let ceiling = (window as f64 * SPLIT_BUFFER_MAX_PCT) as usize;
        let warn_at = (ceiling as f64 * SPLIT_WARN_PCT) as usize;
        let remaining_tokens = self
            .items
            .iter()
            .skip(self.split_cursor_idx())
            .map(|it| it.tokens(self.encoding))
            .sum();
        SplitProjection {
            buffer_tokens,
            ceiling,
            warn_at,
            should_warn: buffer_tokens >= warn_at,
            remaining_tokens,
        }
    }

    /// True when every item has been consumed by the split — the harness then
    /// commits the buffer. Also true when no split is staged (nothing to
    /// commit).
    #[must_use]
    pub fn split_all_consumed(&self) -> bool {
        match self.split.as_ref() {
            None => true,
            Some(_) => self.split_cursor_idx() >= self.items.len(),
        }
    }

    /// Assemble the NEXT chunk of the split: whole items in historical order,
    /// sized to fit `window - SPLIT_CALL_OVERHEAD`, joined in the same
    /// transcript format as the LLM compaction. Never cuts an item — a single
    /// item larger than the budget is still included whole.
    ///
    /// Returns `None` when every item has been consumed (the harness should
    /// commit). The cursor only advances on [`Self::advance_split`], so a
    /// failed call can be retried with the same chunk.
    pub fn split_next_chunk(&mut self) -> Option<SplitChunkRequest> {
        self.split.as_ref()?;
        let start_idx = self.split_cursor_idx();
        if start_idx >= self.items.len() {
            return None;
        }
        let projection = self.split_projection();
        let window = self.split.as_ref().map_or(self.max_tokens, |s| s.window);
        // Reserve the per-call overhead, capped at half the window so a small
        // window (a small fallback model) keeps a usable chunk budget instead
        // of degrading to one item per chunk.
        let overhead = SPLIT_CALL_OVERHEAD.min(window / 2).max(1);
        let budget = window.saturating_sub(overhead).max(1);
        let mut chunk_tokens = 0usize;
        let mut end_idx = start_idx; // exclusive
        for (i, item) in self.items.iter().enumerate().skip(start_idx) {
            let tokens = item.tokens(self.encoding);
            // Always include the first item whole, even when it alone exceeds
            // the budget (structural integrity beats budget precision).
            if end_idx > start_idx && chunk_tokens + tokens > budget {
                break;
            }
            chunk_tokens += tokens;
            end_idx = i + 1;
        }
        let chunk_text = self
            .items
            .iter()
            .skip(start_idx)
            .take(end_idx - start_idx)
            .map(serialize_item)
            .collect::<Vec<String>>()
            .join("\n\n");
        let chunk_end = self.items.get(end_idx - 1).map(ContextItem::id);
        let remaining_tokens: usize = self
            .items
            .iter()
            .skip(end_idx)
            .map(|it| it.tokens(self.encoding))
            .sum();

        // Contenção antecipada: only when the buffer accumulation is
        // concerning do we ask the summarizer to be terse, targeting a
        // proportional share of the remaining ceiling so the final buffer
        // stays under it (with the folga built into the ceiling itself).
        let target_tokens = if projection.should_warn {
            let available = projection.ceiling.saturating_sub(projection.buffer_tokens);
            let target = if remaining_tokens > 0 {
                ((chunk_tokens as f64) * (available as f64) / (remaining_tokens as f64)).ceil()
                    as usize
            } else {
                chunk_tokens
            };
            Some(target.max(64))
        } else {
            None
        };

        let first_chunk = self.split.as_ref().is_none_or(|s| s.continuity.is_empty());
        let continuity = self.split.as_ref().map_or("", |s| s.continuity.as_str());
        let prompt = build_split_prompt(
            first_chunk,
            continuity,
            &chunk_text,
            projection.should_warn,
            target_tokens,
            remaining_tokens,
            projection.buffer_tokens,
        );
        Some(SplitChunkRequest {
            system: SPLIT_SUMMARIZER_SYSTEM.to_string(),
            prompt,
            chunk_end,
        })
    }

    /// Append a successfully summarized chunk to the buffer, extract the
    /// continuity tail, and advance the cursor past `chunk_end`. The timeline
    /// itself stays untouched — this only stages the summary.
    pub fn advance_split(&mut self, summary: &str, chunk_end: Option<u64>) {
        let Some(split) = self.split.as_mut() else {
            return;
        };
        if !summary.trim().is_empty() {
            if !split.buffer.is_empty() {
                split.buffer.push('\n');
            }
            split.buffer.push_str(summary.trim());
            split.buffer_tokens = self.encoding.estimate(&split.buffer);
        }
        // The tail snippet that glues the next chunk onto this one.
        split.continuity = if summary.len() <= SPLIT_CONTINUITY_CHARS {
            summary.to_string()
        } else {
            let start = summary.floor_char_boundary(summary.len() - SPLIT_CONTINUITY_CHARS);
            summary[start..].to_string()
        };
        // Cursor: ids are monotonic, so `chunk_end + 1` is either the next
        // item or past the end (None semantics: `Some(id+1)` with no matching
        // item means "all consumed").
        split.cursor = chunk_end.map(|id| id + 1);
    }

    /// Commit the split atomically: replace the ENTIRE timeline with a single
    /// [`ContextItem::Compaction`] holding the concatenated buffer — the same
    /// end state as a normal LLM compaction. The timeline is only touched
    /// here, and only when every item has been summarized AND the buffer fits
    /// the known window (the split's hard limit — the ceiling is only the
    /// planning target). Returns `false` (leaving everything as it was) when
    /// no split is staged, the buffer is empty, or a pathological overshoot
    /// made the anchor bigger than the window.
    ///
    /// The success criterion is the WINDOW, not the 80% trigger that
    /// [`Self::apply_llm_summary`] reports: that trigger is a normal-compaction
    /// concern (80% of the budget) and must not fail a split whose anchor is
    /// well under the model's window — otherwise the caller would fall back to
    /// the chain drain on an already-committed timeline.
    pub fn commit_split(&mut self) -> bool {
        let Some(split) = self.split.take() else {
            return false;
        };
        if split.buffer.trim().is_empty() {
            return false;
        }
        let fits = self.encoding.estimate(&split.buffer) <= split.window;
        if fits {
            self.apply_llm_summary(split.buffer);
        }
        fits
    }

    /// Abort the split: drop the staging (buffer + cursor). The timeline is
    /// untouched — a failed split leaves the conversation exactly as it was.
    pub fn abort_split(&mut self) {
        self.split = None;
    }

    /// Index of the next un-summarized item (`None` cursor → the beginning).
    fn split_cursor_idx(&self) -> usize {
        match self.split.as_ref().and_then(|s| s.cursor) {
            Some(cid) => self
                .items
                .iter()
                .position(|it| it.id() >= cid)
                .unwrap_or(self.items.len()),
            None => 0,
        }
    }

    // Context-window overflow recovery (driven by the harness)

    /// Remove ONE tool chain (call + result TOGETHER — the native tool-call
    /// format rejects an unpaired half, so `remove_item` always drops the
    /// pair) to relieve a provider-side context-window overflow. The harness
    /// drains one chain per attempt ("1 por vez") and re-requests after each;
    /// when the provider reported its window, the harness also drains locally
    /// with the token estimate to skip the pointless HTTP round trips.
    ///
    /// Removal order (the accepted design): useless chains first, then the
    /// largest (most tokens freed per item), then middle-out as the tiebreak
    /// — the intent at both ends (oldest task start, newest in-flight work)
    /// survives the longest. Unlike the automatic sweep, NO chain is exempt:
    /// when the context still does not fit, everything goes.
    ///
    /// Returns whether a chain was removed; `false` means no tool chain
    /// remains anywhere — the overflow cannot be relieved this way.
    pub fn evict_tool_chain_for_overflow(&mut self) -> bool {
        // Collect the distinct chains (call/result halves share a call_id).
        let mut chains: Vec<(String, usize, bool, usize)> = Vec::new();
        for item in &self.items {
            let (ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. }) =
                item
            else {
                continue;
            };
            if chains.iter().any(|(cid, ..)| cid == call_id) {
                continue;
            }
            let tokens: usize = self
                .items
                .iter()
                .filter(|it| {
                    matches!(
                        it,
                        ContextItem::ToolCall { call_id: c, .. }
                        | ContextItem::ToolResult { call_id: c, .. } if c == call_id
                    )
                })
                .map(|it| it.tokens(self.encoding))
                .sum();
            let useless = self.items.iter().any(|it| {
                matches!(
                    it,
                    ContextItem::ToolResult {
                        call_id: c,
                        useless: true,
                        ..
                    } if c == call_id
                )
            });
            chains.push((call_id.clone(), tokens, useless, 0));
        }
        if chains.is_empty() {
            return false;
        }
        // Middle-out distance from the chain list's middle (the tiebreak).
        let mid = chains.len() / 2;
        for (i, chain) in chains.iter_mut().enumerate() {
            chain.3 = i.abs_diff(mid);
        }
        chains.sort_by(|a, b| {
            b.2.cmp(&a.2) // useless chains first
                .then(b.1.cmp(&a.1)) // then by size, descending
                .then(a.3.cmp(&b.3)) // middle-out tiebreak
        });
        let (call_id, ..) = &chains[0];
        let Some(idx) = self.items.iter().position(|it| {
            matches!(
                it,
                ContextItem::ToolCall { call_id: c, .. }
                | ContextItem::ToolResult { call_id: c, .. } if c == call_id
            )
        }) else {
            return false;
        };
        self.remove_item(idx);
        true
    }

    /// True when the LLM compaction is KNOWN to be stuck for `provider`: a
    /// previous overflow drained every tool chain and the total still exceeds
    /// the window. The harness then skips the doomed summarizer call (it would
    /// only burn a paid request per dispatch) and re-surfaces the notification
    /// until the user switches the model.
    pub fn overflow_stuck(&self, provider: &str) -> bool {
        self.overflow_provider.as_deref() == Some(provider)
    }

    /// Record that the LLM compaction is stuck for `provider` (every tool
    /// chain was drained and the context still exceeds the provider window).
    pub fn mark_overflow(&mut self, provider: &str) {
        self.overflow_provider = Some(provider.to_string());
    }

    /// Forget a recorded stuck overflow — called when the provider changes
    /// (model switch) or a compaction succeeds, so the next overflow starts
    /// fresh.
    pub fn clear_overflow(&mut self) {
        self.overflow_provider = None;
    }

    /// Called by the harness at loop start with the ACTIVE provider: when it
    /// differs from a recorded stuck provider, the user switched the model —
    /// clear the stuck state and retry from scratch.
    pub fn sync_provider(&mut self, provider: &str) {
        if self.overflow_provider.as_deref() != Some(provider) {
            self.clear_overflow();
        }
    }

    // Rendering & persistence

    /// Render the whole conversation as provider-ready messages. One pass over
    /// the items (O(n) once per request); each item maps 1:1 to a message, so
    /// positions are preserved.
    ///
    /// `current_input` is the harness's per-iteration steering message (e.g.
    /// "please continue with the tool results"): it is appended as a final
    /// `user` message, unless it duplicates the trailing user turn (which the
    /// harness already owns via [`add_user`](Self::add_user)).
    pub fn build_messages(&mut self, current_input: &str) -> Vec<ChatMessage> {
        let mut messages = Vec::with_capacity(self.items.len() + 1);
        // Skip leading tool results — a `tool` message right after `system` is
        // rejected by some providers (e.g. Mistral). Orphans should not exist
        // thanks to chain-aware phase removal, but a restored snapshot could
        // still hold them.
        for item in self
            .items
            .iter()
            .skip_while(|it| matches!(it, ContextItem::ToolResult { .. }))
        {
            match item {
                ContextItem::User { original, .. } => {
                    // User prompts are protected: always delivered verbatim.
                    messages.push(user_message(original));
                }
                ContextItem::Assistant {
                    original,
                    compressed,
                    ..
                } => {
                    messages.push(assistant_message(compressed.as_deref().unwrap_or(original)));
                }
                ContextItem::ToolCall {
                    call_id,
                    name,
                    arguments,
                    thought_signature,
                    ..
                } => {
                    messages.push(assistant_tool_call_message(vec![ToolCallMsg {
                        id: call_id.clone(),
                        kind: "function".to_string(),
                        function: ToolCallFunctionMsg {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        thought_signature: (!thought_signature.is_empty())
                            .then(|| thought_signature.clone()),
                    }]));
                }
                ContextItem::ToolResult {
                    call_id, content, ..
                } => {
                    messages.push(tool_result_message(call_id, content));
                }
                ContextItem::LoopClosure { content, .. } => {
                    messages.push(assistant_message(content));
                }
                ContextItem::Compaction { summary, .. } => {
                    messages.push(assistant_message(summary));
                }
            }
        }
        // Append the harness steering input unless it duplicates the trailing
        // user turn (the user prompt already owns it in the CM).
        if !current_input.is_empty()
            && !messages
                .last()
                .is_some_and(|m| m.role == "user" && m.content.as_deref() == Some(current_input))
        {
            messages.push(user_message(current_input));
        }
        // The protected TODO block is injected at the FRONT — right after the
        // system prompt (passed separately by the harness), before the
        // conversation history. It is re-rendered live from the tools' Plan
        // state on every call, so it is structurally immune to the pipeline,
        // the draft eviction, the tool-chain drain and even the LLM
        // compaction (see `todo_ctxt`).
        if let Some(msg) = self.todo.message() {
            messages.insert(0, msg);
        }
        messages
    }

    /// Cheap snapshot for the TUI: total tokens + budget percentage.
    pub fn display_info(&self) -> ContextDisplayInfo {
        let total = self.total_tokens();
        ContextDisplayInfo {
            total_tokens: total,
            budget_pct: if self.max_tokens > 0 {
                ((total as f64 / self.max_tokens as f64) * 100.0).min(100.0) as u8
            } else {
                0
            },
            max_tokens: self.max_tokens,
        }
    }

    /// Snapshot of the current items (test accessor for the harness test
    /// module, which is not a submodule of the context manager).
    #[cfg(test)]
    pub(crate) fn items_snapshot(&self) -> Vec<ContextItem> {
        self.items.iter().cloned().collect()
    }

    /// Serializable snapshot for bincode persistence. The compression
    /// pipeline runs synchronously at the next overflow, so there is no
    /// in-flight work to exclude.
    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            items: self.items.clone(),
            next_id: self.next_id,
            max_tokens: self.max_tokens,
            overflow_provider: self.overflow_provider.clone(),
            split: self.split.clone(),
        }
    }

    /// Restore a previously saved snapshot. The compression pipeline runs
    /// synchronously at the next 80% overflow, so no re-submission is needed —
    /// the items are restored verbatim and the pipeline summarizes them (or
    /// the eviction phases remove them, or the LLM compaction folds them)
    /// exactly as in a fresh session.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.items = state.items.clone();
        self.next_id = state.next_id;
        self.max_tokens = state.max_tokens;
        self.overflow_provider = state.overflow_provider.clone();
        // Split progress is REAL progress (the buffer + cursor) — a restored
        // session resumes the split exactly where it stopped.
        self.split = state.split.clone();
        // Scheduling bias only — a restored session restarts the gradual
        // draft eviction and the segment frontier from the beginning of the
        // timeline.
        self.draft_cursor = None;
        self.segment_start = None;
        // The TODO block mirror is not persisted (the tools' Plan state is
        // not part of the snapshot): clear it so a restored session never
        // surfaces a stale block — the harness re-syncs it at the next loop
        // start from the authoritative Plan.
        self.todo.clear();
    }

    // helpers

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Token count at which the 80% compaction trigger fires.
    fn trigger(&self) -> usize {
        self.max_tokens.saturating_mul(COMPACT_PCT) / 100
    }

    fn total_tokens(&self) -> usize {
        self.items
            .iter()
            .map(|it| it.tokens(self.encoding))
            .sum::<usize>()
            .saturating_add(self.todo.tokens(self.encoding))
    }

    /// Index of the segment frontier's item — where the current segment
    /// starts. `None` (or a removed id) falls back to the beginning of the
    /// timeline; ids are monotonic, so the first item with `id >= frontier`
    /// is the resume point.
    fn segment_start_idx(&self) -> usize {
        self.segment_start
            .map(|sid| {
                self.items
                    .iter()
                    .position(|it| it.id() >= sid)
                    .unwrap_or(self.items.len())
            })
            .unwrap_or(0)
    }

    /// Remove the item at `idx`, returning its token cost. For tool items the
    /// matching call/result partner is removed too, so the native
    /// `tool_call → tool` chain can never break (a lone `tool` message or an
    /// orphaned tool call is rejected by providers).
    fn remove_item(&mut self, idx: usize) -> usize {
        let item = &self.items[idx];
        let call_id = match item {
            ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. } => {
                Some(call_id.clone())
            }
            _ => None,
        };

        let Some(cid) = call_id else {
            let tokens = self.items[idx].tokens(self.encoding);
            self.items.remove(idx);
            return tokens;
        };

        let is_call = matches!(item, ContextItem::ToolCall { .. });
        let partner = self.items.iter().position(|it| match it {
            ContextItem::ToolCall { call_id: c, .. } if !is_call => *c == cid,
            ContextItem::ToolResult { call_id: c, .. } if is_call => *c == cid,
            _ => false,
        });

        match partner {
            // Remove both halves of the chain so the native pairing never
            // breaks. The higher index is removed first so the lower one stays
            // valid; both token costs are summed.
            Some(p) => {
                let (lo, hi) = (idx.min(p), idx.max(p));
                let total =
                    self.items[lo].tokens(self.encoding) + self.items[hi].tokens(self.encoding);
                self.items.remove(hi);
                self.items.remove(lo);
                total
            }
            // No partner (orphan) — remove just this item.
            None => {
                let tokens = self.items[idx].tokens(self.encoding);
                self.items.remove(idx);
                tokens
            }
        }
    }
}
