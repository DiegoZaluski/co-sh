//! Single-owner, synchronous conversation timeline that feeds the LLM.
//!
//! The [`ContextManager`] owns every conversation item — user prompts,
//! assistant outputs, tool calls and tool results — in display order. Content
//! is rendered verbatim; the only compaction is the LLM summarizer: when the
//! held context reaches 80% of the budget, [`Self::run`] returns
//! [`RunOutcome::NeedsLlmCompaction`] and the harness drives the compaction.
//!
//! [`build_messages`](Self::build_messages) renders the timeline as a
//! provider-ready `Vec<ChatMessage>` with a 1:1 item→message mapping. The
//! final text of a finished loop is promoted verbatim into a [`Closure`] by
//! [`close_loop`](Self::close_loop), so the final answer is delivered
//! unchanged.

#[cfg(test)]
mod test;
pub mod todo_ctxt;

use crate::util::TokenEncoding;
use cosh_sdk::connector::{
    ChatMessage, ClaudeThinkingBlock, ToolCallFunctionMsg, ToolCallMsg,
    assistant_tool_call_message, discover_context_window, effective_context_window,
    tool_result_message, user_message,
};
use cosh_tools::plan::types::TodoList;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use todo_ctxt::TodoContext;

/// Default token budget for the total conversation context. Overridable via
/// [`ContextManager::new`].
pub const MAX_CONTEXT_TOKENS: usize = 100_000;

/// Percentage of the budget at which the compaction runs.
const COMPACT_PCT: usize = 80;

// ── Split-and-concatenate (the context-window contingency) ──────────────

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

/// The summarization template the LLM compaction asks the model to fill
/// (opencode's `SUMMARY_TEMPLATE`, kept as inspiration): a structured anchor
/// that preserves the objective, the work state and the next move so the
/// session can continue seamlessly from the summary.
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
/// construction. This is also the persisted representation: the JSONL session
/// log holds one [`ContextItem`] per line (see `session_store`), so the
/// timeline is restored verbatim from disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContextItem {
    /// A user prompt. **Protected**: its original text stays verbatim until
    /// the LLM compaction folds it into the general summary.
    User { id: u64, original: String },
    /// An assistant text output.
    Assistant {
        id: u64,
        original: String,
        /// True when the output carried no tool call, so it may be promoted to
        /// a [`ContextItem::Closure`] by [`ContextManager::close_loop`]. An
        /// output that carried a tool call is structural (never closable).
        closable: bool,
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
        /// Claude extended-thinking blocks that preceded this tool call in the
        /// original response — replayed VERBATIM (text + signature) at the
        /// start of the assistant message in the next request (the API rejects
        /// modified/missing blocks with 400). Empty for every other provider.
        thinking_blocks: Vec<ClaudeThinkingBlock>,
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
    /// assistant text in place.
    Closure { id: u64, content: String },
    /// The continuation summary produced by the LLM compaction (the
    /// last-resort fallback). Replaces the WHOLE timeline in place; renders as
    /// an assistant message. The next LLM compaction folds it into the new
    /// summary (update mode).
    Compaction { id: u64, summary: String },
}

impl ContextItem {
    fn id(&self) -> u64 {
        match self {
            ContextItem::User { id, .. }
            | ContextItem::Assistant { id, .. }
            | ContextItem::ToolCall { id, .. }
            | ContextItem::ToolResult { id, .. }
            | ContextItem::Closure { id, .. }
            | ContextItem::Compaction { id, .. } => *id,
        }
    }

    /// Estimated token cost of what this item currently renders, measured with
    /// the encoding for the active model (`enc` is resolved once by the
    /// manager via [`ContextManager::set_model`], not per item).
    fn tokens(&self, enc: TokenEncoding) -> usize {
        match self {
            ContextItem::User { original, .. } => enc.estimate(original),
            ContextItem::Assistant { original, .. } => enc.estimate(original),
            ContextItem::ToolCall {
                name,
                arguments,
                thought_signature,
                thinking_blocks,
                ..
            } => {
                enc.estimate(name)
                    + enc.estimate(arguments)
                    + enc.estimate(thought_signature)
                    + thinking_blocks
                        .iter()
                        .map(|b| enc.estimate(&b.thinking) + enc.estimate(&b.signature))
                        .sum::<usize>()
            }
            ContextItem::ToolResult { content, .. } => enc.estimate(content),
            ContextItem::Closure { content, .. } => enc.estimate(content),
            ContextItem::Compaction { summary, .. } => enc.estimate(summary),
        }
    }

    fn call_id(&self) -> Option<&str> {
        match self {
            ContextItem::ToolCall { call_id, .. }
            | ContextItem::ToolResult { call_id, .. } => Some(call_id),
            _ => None,
        }
    }
}

// LLM compaction (the last-resort fallback, driven by the harness)

/// Serialize one conversation item into the opencode-style transcript the
/// LLM compaction sends to the model.
fn serialize_item(item: &ContextItem) -> String {
    match item {
        ContextItem::User { original, .. } => format!("[User]: {original}"),
        ContextItem::Assistant { original, .. } => format!("[Assistant]: {original}"),
        ContextItem::ToolCall {
            name, arguments, ..
        } => format!("[Assistant tool call]: {name}({arguments})"),
        // Tool results are passed VERBATIM — the tools bound their own output,
        // and truncating a payload the model still needs only forces a
        // costlier re-read of the source.
        ContextItem::ToolResult { content, .. } => format!("[Tool result]: {content}"),
        ContextItem::Closure { content, .. } => format!("[Assistant]: {content}"),
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

/// Snapshot of context manager state for JSON persistence (the session JSONL
/// log). Nothing is in-flight between save and restore — the snapshot is a
/// plain clone. `overflow_provider` records the stuck context-window overflow,
/// and `split` the staging of an in-progress split-and-concatenate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub max_tokens: usize,
    /// The provider whose context window overflowed the LLM compaction and
    /// could not be relieved by the split. While set (same provider), the
    /// harness skips the doomed summarizer call and only re-notifies — until
    /// the user switches the model or starts a new session. `None` = no known
    /// stuck overflow.
    pub overflow_provider: Option<String>,
    /// Staging of an in-progress split-and-concatenate, so an interrupted
    /// split resumes exactly where it stopped.
    pub split: Option<SplitState>,
}

impl Default for ContextManagerState {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            next_id: 1,
            max_tokens: MAX_CONTEXT_TOKENS,
            overflow_provider: None,
            split: None,
        }
    }
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

/// Outcome of [`ContextManager::run`]: whether the context is within budget,
/// or the harness must run the LLM compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The total is below the 80% trigger — nothing else to do.
    Resolved,
    /// The total is over the trigger: the harness must run the LLM
    /// compaction (the last-resort fallback).
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
    if let Some(target) = target_tokens {
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

/// Orchestrates the single-owner conversation timeline: the useless tool-chain
/// sweep, the `Closure` promotion of each finished agent loop, and the 80%
/// budget trigger. The LLM compaction is requested via
/// [`RunOutcome::NeedsLlmCompaction`] and applied by the harness through
/// [`Self::apply_llm_summary`].
pub struct ContextManager {
    /// Conversation items in display order (oldest first). One item = one
    /// message; positions are stable.
    items: VecDeque<ContextItem>,
    /// Monotonic id counter for items/jobs.
    next_id: u64,
    /// Token budget before the 80% compaction trigger.
    max_tokens: usize,
    /// tiktoken encoding matching the active model (set by the harness via
    /// [`Self::set_model`]). Defaults to [`TokenEncoding::Cl100k`] — the
    /// generic cross-provider estimate.
    encoding: TokenEncoding,
    /// The provider whose context window overflowed the LLM compaction and
    /// could not be relieved by the split (see [`Self::overflow_stuck`]).
    /// Persisted with the snapshot so a stuck session stays notified across
    /// turns until the user switches the model or starts a new session.
    overflow_provider: Option<String>,
    /// Persistent staging of the split-and-concatenate contingency. The
    /// timeline is untouched while it is `Some`; the buffer is committed
    /// atomically by [`Self::commit_split`]. Persisted with the snapshot so an
    /// interrupted split resumes exactly where it stopped.
    split: Option<SplitState>,
    /// The dedicated protected TODO block: mirrors the tools' `Plan` list and
    /// renders it as a protected `user` message at the FRONT of the messages.
    /// Never summarized — see [`todo_ctxt`] for the protection and removal
    /// rules.
    todo: TodoContext,
    /// Running token total of `items` (excluding the TODO block), kept in sync
    /// incrementally by every mutation so `total_tokens()` is O(1) instead of
    /// re-tokenizing every item through tiktoken on each call. Rebuilt
    /// wholesale on restore and on encoding change.
    cached_items_tokens: usize,
    /// Per-item estimated token cost (`id -> tokens`), kept in sync with
    /// `cached_items_tokens`. Lets an in-place edit read the OLD cost of the
    /// edited item without re-tokenizing the whole timeline. Rebuilt wholesale
    /// on restore and on encoding change.
    item_tokens: HashMap<u64, usize>,
    /// Manual-compaction mode (`/compact`): while set, [`Self::trigger`]
    /// returns zero so [`Self::run`] hands off to the LLM summary regardless
    /// of how much budget is free. Scheduling-only — never persisted; a
    /// restored manager is never left mid-manual because the harness brackets
    /// the whole manual pass with
    /// `begin_manual_compaction`/`end_manual_compaction`.
    manual_compaction: bool,
}

/// Build a plain assistant text message (no tool calls).
fn assistant_message(text: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: None,
    }
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            items: VecDeque::new(),
            next_id: 1,
            max_tokens,
            encoding: TokenEncoding::Cl100k,
            overflow_provider: None,
            split: None,
            todo: TodoContext::new(),
            cached_items_tokens: 0,
            item_tokens: HashMap::new(),
            manual_compaction: false,
        }
    }

    /// Create a new ContextManager with automatic context window discovery.
    ///
    /// Attempts to discover the model's context window via public APIs (OpenRouter
    /// and Anthropic). A REAL discovered window is resized to its EFFECTIVE
    /// value — the "sweet spot" budget that sits below the model's degradation
    /// zone (see [`effective_context_window`]). Falls back to
    /// [`MAX_CONTEXT_TOKENS`] if discovery fails; the default is never resized
    /// because it is not a real window.
    ///
    /// # Arguments
    ///
    /// * `model_name` - The model identifier (e.g., "gpt-4o", "claude-sonnet-4-5")
    ///
    /// # Returns
    ///
    /// A new ContextManager with the effective discovered context window, or the
    /// default [`MAX_CONTEXT_TOKENS`] if discovery fails.
    pub async fn with_discovered_context(model_name: &str) -> Self {
        // On-disk catalog caching under `~/.local/share/cosh/cache`, so repeated
        // launches reuse the downloaded models.dev / OpenRouter catalogs.
        let max_tokens = discover_context_window(model_name, Some("cosh/cache"))
            .await
            // Only a REAL discovered window is resized to its effective value;
            // the default fallback stays untouched (it is not a real window).
            .map(effective_context_window)
            .unwrap_or(MAX_CONTEXT_TOKENS);

        Self::new(max_tokens)
    }

    /// Set the active model so token counts are estimated with the closest
    /// tiktoken encoding (`None` → the default [`TokenEncoding::Cl100k`]). The
    /// harness calls this whenever the connector/model changes (loop start,
    /// fallback switch).
    pub fn set_model(&mut self, model: Option<&str>) {
        let new = TokenEncoding::for_model(model);
        if new == self.encoding {
            // The harness calls this at every loop start; on a large session
            // recomputing when nothing changed would re-pay the O(n) tiktoken
            // pass this cache exists to avoid.
            return;
        }
        self.encoding = new;
        // A different tokenizer changes EVERY item's estimated cost — the
        // cached total is stale.
        self.recompute_cached_tokens();
    }

    /// Override the token budget, e.g. from context-window discovery for the
    /// active model ([`Self::with_discovered_context`]). The 80% compaction
    /// trigger ([`Self::run`]) re-scales with it, so the LLM compaction fires
    /// at a sane fraction of the model's REAL window instead of a hardcoded
    /// default that is far below it.
    ///
    /// Callers passing a DISCOVERED window should pass its effective value
    /// ([`effective_context_window`]) — the advertised window is not what the
    /// model can reason over. The default budget ([`MAX_CONTEXT_TOKENS`]) is
    /// never resized; a restored snapshot passes back the already-effective
    /// value it persisted.
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

    /// Add a user prompt. Protected by construction: its original text stays
    /// verbatim until the LLM compaction folds it into the general summary.
    pub fn add_user(&mut self, text: &str) {
        let id = self.next_id();
        self.push_item(ContextItem::User {
            id,
            original: text.to_string(),
        });
    }

    /// Add an assistant text output. When `closable` is true the text is a
    /// pure-text turn (no tool call) and may be promoted to a [`Closure`] by
    /// [`Self::close_loop`].
    pub fn add_assistant(&mut self, text: &str, closable: bool) {
        let id = self.next_id();
        self.push_item(ContextItem::Assistant {
            id,
            original: text.to_string(),
            closable,
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
        self.add_tool_call_with_thinking(call_id, name, arguments, thought_signature, Vec::new());
    }

    /// Add a tool CALL carrying the Claude extended-thinking blocks that
    /// preceded it in the original response (replayed verbatim — text +
    /// signature — in the next request's history; the API rejects modified or
    /// missing blocks with 400). Structural, never prose-compressed. Every
    /// non-Claude path passes an empty list.
    pub fn add_tool_call_with_thinking(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        thought_signature: &str,
        thinking_blocks: Vec<ClaudeThinkingBlock>,
    ) {
        let id = self.next_id();
        self.push_item(ContextItem::ToolCall {
            id,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: thought_signature.to_string(),
            thinking_blocks,
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
        self.push_item(ContextItem::ToolResult {
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
    /// tool work, Closure or summary), every turn but the newest is an input
    /// the user gave up on — the run ended without the LLM producing anything
    /// (typically cancelled with Esc), and a new input followed. Each older
    /// turn of the run is removed, so the model only ever sees the newest
    /// input. A turn that produced ANY output after it is never touched.
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
        for it in self.items.range(start..end) {
            // Read the cost from the per-item cache instead of re-tokenizing
            // (the abandoned inputs are dropped wholesale here).
            let id = it.id();
            let tokens = self
                .item_tokens
                .remove(&id)
                .unwrap_or_else(|| it.tokens(self.encoding));
            self.cached_items_tokens = self.cached_items_tokens.saturating_sub(tokens);
        }
        self.items.drain(start..end);
        true
    }

    /// Promote the last assistant text output of the finished agent loop into a
    /// [`ContextItem::Closure`], replacing the raw text in place. Guard: if
    /// the last item is not an assistant text (e.g. it was a tool call), it
    /// cannot be a summary of what was done, so no `Closure` is created.
    ///
    /// The harness has now told us this turn was the loop's FINAL output: the
    /// ORIGINAL text is promoted verbatim, so the final answer is never
    /// summarized away.
    pub fn close_loop(&mut self) {
        let Some(ContextItem::Assistant { id, original, closable }) = self.items.back() else {
            return;
        };
        if !*closable {
            return;
        }
        let draft_id = *id;
        let content = original.clone();
        let draft_tokens = self
            .item_tokens
            .remove(&draft_id)
            .unwrap_or_else(|| self.encoding.estimate(&content));
        self.items.pop_back();
        self.cached_items_tokens = self.cached_items_tokens.saturating_sub(draft_tokens);
        self.push_item(ContextItem::Closure {
            id: draft_id,
            content,
        });
    }

    /// The final text of a completed agent loop: the `Closure` content of the
    /// last item, set by [`Self::close_loop`]. `None` when the loop was not
    /// closed or the last item is not the closure (e.g. the last turn ended in
    /// a tool call).
    #[must_use]
    pub fn final_answer(&self) -> Option<String> {
        match self.items.back() {
            Some(ContextItem::Closure { content, .. }) => Some(content.clone()),
            _ => None,
        }
    }

    // Compaction

    /// Per-iteration tick: run the useless tool-chain sweep, then check the
    /// 80% budget trigger.
    ///
    /// When the total is at or over the trigger, returns
    /// [`RunOutcome::NeedsLlmCompaction`] so the harness runs the LLM
    /// compaction and applies the summary via [`Self::apply_llm_summary`].
    pub fn run(&mut self) -> RunOutcome {
        self.sweep_useless_chains();
        if self.total_tokens() < self.trigger() {
            RunOutcome::Resolved
        } else {
            RunOutcome::NeedsLlmCompaction
        }
    }

    /// Remove every tool chain whose result is marked `useless`, except the
    /// newest chain (the one the model has not reacted to yet). Runs at the
    /// start of every [`Self::run`], regardless of the budget.
    fn sweep_useless_chains(&mut self) {
        // The newest chain (the one the model has not seen yet) is preserved:
        // its CALL id is the chain identity, so BOTH halves are excluded.
        let newest_call_id: Option<String> = self
            .items
            .iter()
            .rev()
            .find_map(|it| it.call_id().map(str::to_string));
        let useless_call_ids: HashSet<String> = self
            .items
            .iter()
            .filter_map(|it| match it {
                ContextItem::ToolResult {
                    call_id,
                    useless: true,
                    ..
                } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        // Back-to-front so removing a chain never invalidates a pending index;
        // remove_item also drops the partner half, so its index is skipped
        // naturally by the next iteration.
        let mut idx = self.items.len();
        while idx > 0 {
            idx -= 1;
            let doomed = match &self.items[idx] {
                ContextItem::ToolCall { call_id, .. }
                | ContextItem::ToolResult { call_id, .. } => {
                    newest_call_id.as_deref() != Some(call_id.as_str())
                        && useless_call_ids.contains(call_id)
                }
                _ => false,
            };
            if doomed {
                self.remove_item(idx);
            }
        }
    }

    /// Build the LLM-compaction request: the entire remaining context —
    /// protected items included — serialized into an opencode-style
    /// transcript, wrapped in the summarization prompt.
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
    /// single [`ContextItem::Compaction`] (the continuation summary). Returns
    /// whether the total is now below the 80% trigger — the summary should be
    /// small enough, but a degenerate huge summary is surfaced so the harness
    /// can report failure. The verdict always uses the NORMAL trigger (manual
    /// `/compact` mode zeroes [`Self::trigger`] only for the run; a manual pass
    /// legitimately lands far below it).
    pub fn apply_llm_summary(&mut self, summary: String) -> bool {
        let id = self.next_id();
        self.items.clear();
        // The whole timeline is gone — reset both caches before pushing the
        // single replacement item.
        self.cached_items_tokens = 0;
        self.item_tokens.clear();
        self.push_item(ContextItem::Compaction { id, summary });
        // A successful compaction is proof the provider accepts the context
        // again — any recorded stuck overflow is no longer relevant.
        self.clear_overflow();
        self.total_tokens() < self.normal_trigger()
    }

    // Split-and-concatenate (the context-window contingency, driven by the
    // harness). The timeline is untouched until commit_split.

    /// The current token budget — the discovered window or the default. The
    /// harness uses it (with the error-reported window) to detect the
    /// context-exceeds-window split trigger.
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
    /// vs the ceiling.
    #[must_use]
    pub fn split_projection(&self) -> SplitProjection {
        let split = self.split.as_ref();
        let window = split.map_or(self.max_tokens, |s| s.window);
        let buffer_tokens = split.map_or(0, |s| s.buffer_tokens);
        let ceiling = (window as f64 * SPLIT_BUFFER_MAX_PCT) as usize;
        let warn_at = (ceiling as f64 * SPLIT_WARN_PCT) as usize;
        SplitProjection {
            buffer_tokens,
            ceiling,
            warn_at,
            should_warn: buffer_tokens >= warn_at,
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

        // Early containment: only when the buffer accumulation is concerning
        // do we ask the summarizer to be terse, targeting a proportional share
        // of the remaining ceiling so the final buffer stays under it (the
        // slack is built into the ceiling itself).
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
        let summary = summary.trim();
        // An empty summary means the chunk was never summarized: advancing the
        // cursor would silently drop those items from the final anchor.
        if summary.is_empty() {
            return;
        }
        if !split.buffer.is_empty() {
            split.buffer.push('\n');
        }
        split.buffer.push_str(summary);
        split.buffer_tokens = self.encoding.estimate(&split.buffer);

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

    /// True when the LLM compaction is KNOWN to be stuck for `provider`: a
    /// previous overflow could not be relieved by the split and the total
    /// still exceeds the window. The harness then skips the doomed summarizer
    /// call (it would only burn a paid request per dispatch) and re-surfaces
    /// the notification until the user switches the model.
    pub fn overflow_stuck(&self, provider: &str) -> bool {
        self.overflow_provider.as_deref() == Some(provider)
    }

    /// Record that the LLM compaction is stuck for `provider` (the split could
    /// not fit the context into the provider window).
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
                ContextItem::Assistant { original, .. } => {
                    messages.push(assistant_message(original));
                }
                ContextItem::ToolCall {
                    call_id,
                    name,
                    arguments,
                    thought_signature,
                    thinking_blocks,
                    ..
                } => {
                    let mut msg = assistant_tool_call_message(vec![ToolCallMsg {
                        id: call_id.clone(),
                        kind: "function".to_string(),
                        function: ToolCallFunctionMsg {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        thought_signature: (!thought_signature.is_empty())
                            .then(|| thought_signature.clone()),
                    }]);
                    // Claude extended-thinking blocks travel with the tool
                    // call they preceded (the Claude caller replays them
                    // verbatim at the start of the assistant message).
                    if !thinking_blocks.is_empty() {
                        msg.thinking_blocks = Some(thinking_blocks.clone());
                    }
                    messages.push(msg);
                }
                ContextItem::ToolResult {
                    call_id, content, ..
                } => {
                    messages.push(tool_result_message(call_id, content));
                }
                ContextItem::Closure { content, .. } => {
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
        // state on every call, so it is structurally immune to even the LLM
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

    /// Serializable snapshot for JSON persistence (the session JSONL log).
    /// Nothing is in-flight between save and restore — the snapshot is a plain
    /// clone.
    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            items: self.items.clone(),
            next_id: self.next_id,
            max_tokens: self.max_tokens,
            overflow_provider: self.overflow_provider.clone(),
            split: self.split.clone(),
        }
    }

    /// Restore a previously saved snapshot. The items are restored verbatim;
    /// the next 80% overflow triggers the LLM compaction exactly as in a fresh
    /// session.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.items = state.items.clone();
        self.next_id = state.next_id;
        self.max_tokens = state.max_tokens;
        self.overflow_provider = state.overflow_provider.clone();
        // Split progress is REAL progress (the buffer + cursor) — a restored
        // session resumes the split exactly where it stopped.
        self.split = state.split.clone();
        // The TODO block mirror is not persisted (the tools' Plan state is
        // not part of the snapshot): clear it so a restored session never
        // surfaces a stale block — the harness re-syncs it at the next loop
        // start from the authoritative Plan.
        self.todo.clear();
        // The items were replaced wholesale — rebuild the cached total.
        self.recompute_cached_tokens();
    }

    // helpers

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Push an item and keep the cached token total in sync (O(1) instead of
    /// re-tokenizing the whole timeline). The cache is the single source of
    /// truth for `total_tokens()`; every mutation of `items` must update it.
    fn push_item(&mut self, item: ContextItem) {
        // The item is tokenized HERE for the running total — keep the per-item
        // cost so a later in-place edit can read the old cost without
        // re-tokenizing it.
        let tokens = item.tokens(self.encoding);
        self.item_tokens.insert(item.id(), tokens);
        self.cached_items_tokens = self.cached_items_tokens.saturating_add(tokens);
        self.items.push_back(item);
    }

    /// Rebuild the cached token total (and the per-item cache) from scratch.
    /// Called when the encoding changes (every cached per-item cost is then
    /// stale) and on restore.
    fn recompute_cached_tokens(&mut self) {
        self.item_tokens.clear();
        self.cached_items_tokens = 0;
        for it in self.items.iter() {
            let tokens = it.tokens(self.encoding);
            self.item_tokens.insert(it.id(), tokens);
            self.cached_items_tokens = self.cached_items_tokens.saturating_add(tokens);
        }
    }

    /// Token count at which the 80% compaction trigger fires: 80% of the
    /// budget normally, ZERO while the manual `/compact` mode is active — so
    /// [`Self::run`] hands off to the LLM summary regardless of how much
    /// budget is free. The LLM-side guards ([`Self::llm_compaction_request`],
    /// [`Self::apply_llm_summary`]) keep their normal semantics (see those).
    fn trigger(&self) -> usize {
        if self.manual_compaction {
            0
        } else {
            self.normal_trigger()
        }
    }

    fn normal_trigger(&self) -> usize {
        self.max_tokens.saturating_mul(COMPACT_PCT) / 100
    }

    /// Enter manual-compaction mode (`/compact`): the next [`Self::run`]
    /// returns [`RunOutcome::NeedsLlmCompaction`] whenever anything is left,
    /// instead of stopping at the 80% trigger. Always paired with
    /// [`Self::end_manual_compaction`] after the LLM pass.
    pub fn begin_manual_compaction(&mut self) {
        self.manual_compaction = true;
    }

    /// Leave manual-compaction mode. Idempotent.
    pub fn end_manual_compaction(&mut self) {
        self.manual_compaction = false;
    }

    /// True when there is anything worth compacting: at least one timeline
    /// item beyond a lone previous summary (a session that only holds the
    /// last `/compact` result has nothing new to fold).
    pub fn has_compactable_content(&self) -> bool {
        !self.items.is_empty()
            && !(self.items.len() == 1
                && matches!(self.items.front(), Some(ContextItem::Compaction { .. })))
    }

    fn total_tokens(&self) -> usize {
        // Debug-only invariant: the incremental caches have 13+ sync points,
        // so a future mutation that forgets to update them would silently
        // corrupt compaction timing (saturating arithmetic masks underflow).
        // Every call to `total_tokens` — which the whole test suite makes
        // constantly — cross-checks the cached total against a brute-force
        // recompute AND the per-item map against the items themselves in
        // debug builds, failing fast on drift. Compiled out in release: zero
        // cost.
        #[cfg(debug_assertions)]
        {
            let brute: usize = self.items.iter().map(|it| it.tokens(self.encoding)).sum();
            debug_assert_eq!(
                self.cached_items_tokens, brute,
                "cached token total drifted from items ({}, expected {})",
                self.cached_items_tokens, brute
            );
            // The per-item map must mirror every item 1:1 — a stale entry
            // would silently feed wrong deltas into pipeline/trim/eviction.
            debug_assert_eq!(
                self.item_tokens.len(),
                self.items.len(),
                "per-item token cache drifted: {} entries for {} items",
                self.item_tokens.len(),
                self.items.len()
            );
            for it in self.items.iter() {
                let cached = self.item_tokens.get(&it.id());
                debug_assert_eq!(
                    cached,
                    Some(&it.tokens(self.encoding)),
                    "per-item token cache drifted for item {}",
                    it.id()
                );
            }
        }
        self.cached_items_tokens
            .saturating_add(self.todo.tokens(self.encoding))
    }

    /// Remove the item at `idx`. For tool items the matching call/result
    /// partner is removed too, so the native `tool_call → tool` chain can
    /// never break (a lone `tool` message or an orphaned tool call is
    /// rejected by providers).
    fn remove_item(&mut self, idx: usize) {
        let is_call = matches!(self.items[idx], ContextItem::ToolCall { .. });
        let Some(cid) = self.items[idx].call_id().map(str::to_string) else {
            self.remove_at(idx);
            return;
        };
        let partner = self.items.iter().position(|it| match it {
            ContextItem::ToolCall { call_id: c, .. } if !is_call => *c == cid,
            ContextItem::ToolResult { call_id: c, .. } if is_call => *c == cid,
            _ => false,
        });
        match partner {
            // Remove both halves so the native pairing never breaks; the
            // higher index first so the lower one stays valid.
            Some(p) => {
                self.remove_at(idx.max(p));
                self.remove_at(idx.min(p));
            }
            // No partner (orphan) — remove just this item.
            None => self.remove_at(idx),
        }
    }

    /// Remove the item at `idx` and adjust the cached token total, reading the
    /// cost from the per-item cache instead of re-tokenizing (removed content
    /// can be large — e.g. a whole tool result).
    fn remove_at(&mut self, idx: usize) {
        let id = self.items[idx].id();
        let tokens = self
            .item_tokens
            .remove(&id)
            .unwrap_or_else(|| self.items[idx].tokens(self.encoding));
        self.items.remove(idx);
        self.cached_items_tokens = self.cached_items_tokens.saturating_sub(tokens);
    }
}
