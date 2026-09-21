//! Single-owner, synchronous conversation timeline that feeds the LLM.
//!
//! The [`ContextManager`] owns every conversation item — user prompts,
//! assistant outputs, tool calls and tool results — in display order. Content
//! is rendered through a recoverable layered view. The model can request that
//! a tool result be replaced by a typed reference (tail masking, see
//! [`ContextManager::mask_newest_tool_result`]), a recent token window stays
//! verbatim, and an LLM checkpoint covers the remaining old prefix when the
//! view still reaches 80% of the budget.
//!
//! [`build_messages`](Self::build_messages) renders the timeline as a
//! provider-ready `Vec<ChatMessage>` with a 1:1 item→message mapping. The
//! final text of a finished loop is promoted verbatim into a [`Closure`] by
//! [`close_loop`](Self::close_loop), so it is delivered word-for-word for as
//! long as it remains in the timeline.

mod error_catalog;
mod map_reduce;
mod split;
mod summarize;

pub use error_catalog::error_catalog_window;
pub use map_reduce::{
    MapReducePhase, MapReduceState, MapRequest, MapSegment, ReduceRequest, SegmentSlice,
    SummaryNode, ValidationRequest,
};
pub use split::{SplitChunkRequest, SplitState};
use summarize::{SUMMARIZER_SYSTEM, build_llm_prompt, serialize_item};
#[cfg(test)]
#[cfg(test)]
mod test;
mod todo_ctxt;

use crate::util::TokenEncoding;
use cosh_sdk::connector::{
    ChatMessage, ClaudeThinkingBlock, ToolCallFunctionMsg, ToolCallMsg,
    assistant_tool_call_message, discover_context_window, effective_context_window,
    tool_result_message, user_message,
};
use cosh_tools::plan::types::TodoList;
use error_catalog::save_error_catalog_window;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use todo_ctxt::TodoContext;

/// Default token budget for the total conversation context. Overridable via
/// [`ContextManager::new`].
pub const MAX_CONTEXT_TOKENS: usize = 100_000;

/// Percentage of the budget at which the compaction runs.
const COMPACT_PCT: usize = 80;

/// Percentage of the model window reserved for the most recent conversation
/// turn in verbatim form. Older content is eligible for checkpointing; the
/// newest turn remains a high-fidelity handoff after compaction.
const RECENT_RAW_PCT: usize = 20;

/// Inclusive range of monotonic context item IDs covered by a derived
/// checkpoint. Multiple ranges are used when already-hidden items leave gaps,
/// so the metadata describes exactly which visible source items were folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItemRange {
    pub start_id: u64,
    pub end_id: u64,
}

/// A single conversation item in display order. **One item = one message** in
/// [`ContextManager::build_messages`], so message positions are preserved by
/// construction. The TUI session store persists item additions/replacements
/// as immutable JSONL deltas, so this runtime projection is rebuilt verbatim
/// from event history.
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
        /// other provider. Defaults for contexts persisted before this field.
        #[serde(default)]
        thought_signature: String,
        /// Claude extended-thinking blocks that preceded this tool call in the
        /// original response — replayed VERBATIM (text + signature) at the
        /// start of the assistant message in the next request (the API rejects
        /// modified/missing blocks with 400). Empty for every other provider.
        /// Defaults for contexts persisted before this field.
        #[serde(default)]
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
        /// [`Self::run`] removes it without waiting for a budget overflow.
        /// The newest chain is kept until the model reacts — an explicit
        /// `mask_tool_result` call counts as that reaction.
        useless: bool,
    },
    /// The final text response of a completed agent loop. Replaces the raw
    /// assistant text in place.
    Closure { id: u64, content: String },
    /// A derived continuation checkpoint produced by LLM compaction. It is
    /// appended to immutable history and records the exact ranges it covers;
    /// model composition places it before the recent raw tail. A later
    /// checkpoint folds the prior checkpoint and carries its coverage forward.
    Compaction {
        id: u64,
        summary: String,
        /// Exact visible source ranges folded into this checkpoint. Empty for
        /// legacy compaction records written before range metadata existed.
        #[serde(default)]
        covered_ranges: Vec<ContextItemRange>,
    },
    /// A terminal API/runtime error surfaced to the user. DISPLAY-ONLY:
    /// [`ContextManager::build_messages`] skips it (an API failure must never
    /// reach the LLM as if it were assistant output — the live loop keeps the
    /// same contract), it costs zero tokens, and the summarizers never see it.
    /// It exists in the timeline solely so the persisted transcript restores
    /// the error line with its own display semantics (the TUI's styled error
    /// box) instead of collapsing it into plain assistant prose.
    Error { id: u64, content: String },
    /// A shell command typed by the USER in Command mode (the TUI as a plain
    /// terminal). DISPLAY-ONLY, like [`ContextItem::Error`]: zero tokens,
    /// skipped by [`ContextManager::build_messages`] and the summarizers —
    /// the model never sees it. Named `UserCommand` (not `Command`) because
    /// the agent itself has a separate notion of shell commands (bash tool
    /// calls) and there must be no ambiguity. Persisted so the transcript
    /// restores the command line; recorded hidden from birth.
    UserCommand { id: u64, content: String },
}

impl ContextItem {
    pub fn id(&self) -> u64 {
        match self {
            ContextItem::User { id, .. }
            | ContextItem::Assistant { id, .. }
            | ContextItem::ToolCall { id, .. }
            | ContextItem::ToolResult { id, .. }
            | ContextItem::Closure { id, .. }
            | ContextItem::Compaction { id, .. }
            | ContextItem::Error { id, .. }
            | ContextItem::UserCommand { id, .. } => *id,
        }
    }

    /// Estimated token cost of what this item currently renders, measured with
    /// the encoding for the active model (`enc` is resolved once by the
    /// manager via [`ContextManager::set_model`], not per item).
    fn tokens(&self, enc: TokenEncoding) -> usize {
        match self {
            ContextItem::User { original, .. } | ContextItem::Assistant { original, .. } => {
                enc.estimate(original)
            }
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
            // Display-only: never delivered to the model, never budgeted.
            ContextItem::Error { .. } => 0,
            // User-typed Command-mode command: display-only for the model
            // (the TUI is a plain terminal there), zero token cost.
            ContextItem::UserCommand { .. } => 0,
        }
    }

    fn call_id(&self) -> Option<&str> {
        match self {
            ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. } => {
                Some(call_id)
            }
            _ => None,
        }
    }
}

// LLM compaction (the last-resort fallback, driven by the harness)

/// Snapshot of context manager state, carried by the harness events
/// (`Done`/`Stopped`/`Error`/`ContextSnapshot`) and handed to the TUI for
/// persistence. Nothing is in-flight between save and restore — the snapshot
/// is a plain clone. `overflow_model` records the stuck context-window
/// overflow, `map_reduce` the current checkpoint-construction staging, and
/// `split` any legacy split-and-concatenate staging, plus the model-view
/// selectors.
///
/// The timeline is APPEND-ONLY: nothing is ever removed by compaction, the
/// useless-chain sweep or the abandoned-input cleanup. Those events only move
/// the VISIBILITY markers (`visible_from` boundary + `hidden` set), so the
/// full history stays on disk for revert/fork at any point of the session —
/// the model's view is exactly what the markers say.
///
/// Persistence is the TUI's concern (`session_store`): it compares this
/// transport projection with the replayed branch and appends fine-grained
/// context deltas. This value is never an authoritative on-disk snapshot.
/// The field is keyed by MODEL, not provider: the
/// stuck constraint is the model's window, so a model switch inside the same
/// provider must clear it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub max_tokens: usize,
    /// The model whose context window overflowed the LLM compaction and
    /// could not be relieved by the split. While set (same model), the
    /// harness skips the doomed summarizer call and only re-notifies — until
    /// the user switches the model or starts a new session. `None` = no known
    /// stuck overflow.
    pub overflow_model: Option<String>,
    /// Staging of an in-progress split-and-concatenate, so an interrupted
    /// split written by an older version resumes exactly where it stopped.
    pub split: Option<SplitState>,
    /// Range-addressed, resumable staging for hierarchical checkpoint
    /// construction. It contains references and derived summaries, never a
    /// second copy of the authoritative raw transcript.
    #[serde(default)]
    pub map_reduce: Option<MapReduceState>,
    /// Legacy compaction boundary. New checkpoints derive coverage from their
    /// own exact ranges, but this remains for replaying histories written by
    /// older versions without rewriting them.
    pub visible_from: Option<u64>,
    /// Item ids hidden from the model one by one (useless-chain sweep,
    /// abandoned prompts, cancelled-run debris). Kept in the timeline for
    /// revert/fork; never sent to the model. Ids below `visible_from` are
    /// implicitly hidden too and are pruned from here on compaction.
    pub hidden: HashSet<u64>,
    /// Tool result IDs whose full payload remains in `items` and immutable
    /// history, but whose model-facing projection is a small typed reference.
    /// Only the model's explicit `mask_tool_result` requests land here —
    /// masking is never applied deterministically, because a mid-history
    /// mutation would invalidate the providers' exact-prefix caches.
    #[serde(default)]
    pub masked: HashSet<u64>,
    /// Structured protected plan reconstructed from session deltas. Absent
    /// in legacy histories; empty and terminal plans remain explicit state.
    #[serde(default)]
    pub todo: Option<TodoList>,
    /// The effective tool set the model was last offered (sorted, deduplicated
    /// names). Persisted across turns so the harness can notify the model when
    /// the available tools change (mode switch, disabled-tool change, MCP
    /// server drift). `None` when the session has never recorded a tool set.
    #[serde(default)]
    pub last_tool_set: Option<Vec<String>>,
}

impl Default for ContextManagerState {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            next_id: 1,
            max_tokens: MAX_CONTEXT_TOKENS,
            overflow_model: None,
            split: None,
            map_reduce: None,
            visible_from: None,
            hidden: HashSet::new(),
            masked: HashSet::new(),
            todo: None,
            last_tool_set: None,
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
    /// transcript of the selected old prefix; the recent raw tail is omitted.
    pub prompt: String,
}

/// The handoff/tool-set consistency hook a harness installs on its context
/// manager: rewrites a compaction summary before it is committed (see
/// `annotate_summary_tool_set` in the harness core).
type SummaryAnnotator = dyn Fn(&str) -> String + Send + Sync;

/// Orchestrates the single-owner conversation timeline: the useless tool-chain
/// sweep, the `Closure` promotion of each finished agent loop, and the 80%
/// budget trigger. The LLM compaction is requested via
/// [`RunOutcome::NeedsLlmCompaction`] and applied by the harness through
/// [`Self::apply_llm_summary`].
pub struct ContextManager {
    binding_tx: Option<tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>>,
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
    /// The model whose context window overflowed the LLM compaction and
    /// could not be relieved by the split (see [`Self::overflow_stuck`]).
    /// Included in the transport projection and persisted as a context delta,
    /// so a stuck session stays notified across turns until the user switches
    /// the model or starts a new session.
    overflow_model: Option<String>,
    /// Legacy persistent staging for split-and-concatenate histories. New
    /// contingencies use range-addressed MapReduce, but an interrupted legacy
    /// split must remain resumable after an upgrade.
    split: Option<SplitState>,
    /// Persistent, range-addressed MapReduce staging. The committed model view
    /// is untouched while this is `Some`; only a validated final summary is
    /// appended as a checkpoint.
    map_reduce: Option<MapReduceState>,
    /// Legacy compaction boundary retained for backward-compatible replay.
    /// New checkpoint coverage is derived from `Compaction::covered_ranges`.
    visible_from: Option<u64>,
    /// Item ids hidden from the model one by one (useless-chain sweep,
    /// abandoned prompts, cancelled-run debris). Kept in the timeline for
    /// revert/fork; never sent to the model. Ids below `visible_from` are
    /// implicitly hidden too and are pruned from here on compaction.
    hidden: HashSet<u64>,
    /// Tool results the model explicitly asked to mask (the harness
    /// `mask_tool_result` tool): represented by small typed placeholders in
    /// the model view. The original result remains untouched in `items`.
    /// Never filled deterministically — see the `masked` state field note.
    masked: HashSet<u64>,
    /// Derived coverage of the newest base-visible checkpoint. Rebuilt from
    /// immutable checkpoint metadata on restore; never persisted separately.
    checkpoint_coverage: Vec<ContextItemRange>,
    /// The dedicated protected TODO block: mirrors the tools' `Plan` list and
    /// renders it as a protected block at the END of the messages (merged into
    /// the trailing `user` turn when there is one — block first, steering
    /// after — so no two consecutive `user` messages ever reach the provider).
    /// The tail placement keeps the stable `[system → history]` prefix cached
    /// across `plan_*` re-renders. Never summarized — see [`todo_ctxt`] for
    /// the protection and removal rules.
    todo: TodoContext,
    /// Transient tail block with the tool-correction memory, mirrored by the
    /// harness before every request. Corrections churn constantly (new
    /// failure, duplicate reordering, eviction), so they must NEVER live in
    /// the system prompt: any change there invalidates the exact-prefix cache
    /// of the WHOLE conversation. Rendered at the tail — before the TODO
    /// block — a change only costs the block itself. Like the TODO block, it
    /// is not an item: it is re-mirrored per request and never compacted.
    /// Never persisted; a restored manager starts with an empty block.
    correction_block: Option<String>,
    /// Memoized token estimate of [`Self::correction_block`], kept in sync by
    /// [`Self::set_correction_block`] so `total_tokens` stays O(1).
    correction_block_tokens: usize,
    /// Transient rehydration signal, never authoritative or persisted state.
    todo_restore_pending: bool,
    /// Handoff/tool-set consistency check: applied to every summary at
    /// commit time (single-shot, MapReduce, and legacy split paths all
    /// funnel through [`Self::commit_summary_to_source`]). The harness owns
    /// the tool-set knowledge and re-installs a closure capturing a fresh
    /// snapshot; `None` leaves summaries untouched.
    summary_annotator: Option<Box<SummaryAnnotator>>,
    /// Running token total of `items` (excluding the TODO block), kept in sync
    /// by the few mutation primitives (`push_item`, `hide_at`,
    /// `apply_llm_summary`, `recompute_cached_tokens`) so `total_tokens()` is
    /// O(1) instead of re-tokenizing every item through tiktoken on each call.
    /// Rebuilt wholesale on restore and on encoding change. There is NO
    /// per-item map: a removal re-tokenizes the doomed item (removals are
    /// rare and bounded), which keeps the cache-to-items sync trivial and
    /// drift — the failure mode that motivated a debug-only O(n) invariant —
    /// structurally out of reach.
    cached_items_tokens: usize,
    /// Manual-compaction mode (`/compact`): while set, [`Self::trigger`]
    /// returns zero so [`Self::run`] hands off to the LLM summary regardless
    /// of how much budget is free. Scheduling-only — never persisted; a
    /// restored manager is never left mid-manual because the harness brackets
    /// the whole manual pass with
    /// `begin_manual_compaction`/`end_manual_compaction`.
    manual_compaction: bool,
    /// The effective tool set the model was last offered (sorted,
    /// deduplicated names). Set by the harness at header build; read back on
    /// the next turn to detect tool-set drift. `None` on a brand-new session.
    last_tool_set: Option<Vec<String>>,
}

/// Build a plain assistant text message (no tool calls).
fn assistant_message(text: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: None,
        images: None,
    }
}

fn masked_tool_result(id: u64) -> String {
    format!(
        "[Historical tool result masked from the active view; source context item #{id} remains in the immutable session history.]"
    )
}

/// Append a Markdown section to the transcript buffer: heading, blank line,
/// body, blank line. Adjacent sections stay visually separated.
fn push_section(out: &mut String, heading: &str, body: &str) {
    out.push_str(heading);
    out.push_str("\n\n");
    out.push_str(body.trim_end_matches('\n'));
    out.push_str("\n\n");
}

/// Append a fenced code block whose fence is longer than any backtick run in
/// the body, so payloads that contain Markdown fences of their own (diffs,
/// docs, transcripts) never break out of the block.
fn push_fenced(out: &mut String, lang: &str, body: &str) {
    let longest = body
        .split('\n')
        .map(|line| line.chars().take_while(|c| *c == '`').count())
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    out.push_str(&fence);
    out.push_str(lang);
    out.push('\n');
    out.push_str(body.trim_matches('\n'));
    out.push('\n');
    out.push_str(&fence);
    out.push('\n');
}

pub(super) fn coalesce_ranges(ids: &[u64]) -> Vec<ContextItemRange> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut ranges: Vec<ContextItemRange> = Vec::new();
    for id in ids {
        match ranges.last_mut() {
            Some(last) if last.end_id.checked_add(1) == Some(id) => last.end_id = id,
            _ => ranges.push(ContextItemRange {
                start_id: id,
                end_id: id,
            }),
        }
    }
    ranges
}

pub(super) fn merge_ranges(mut ranges: Vec<ContextItemRange>) -> Vec<ContextItemRange> {
    ranges.sort_unstable_by_key(|range| (range.start_id, range.end_id));
    let mut merged: Vec<ContextItemRange> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last)
                if range.start_id <= last.end_id
                    || last.end_id.checked_add(1) == Some(range.start_id) =>
            {
                last.end_id = last.end_id.max(range.end_id);
            }
            _ => merged.push(range),
        }
    }
    merged
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            binding_tx: None,
            items: VecDeque::new(),
            next_id: 1,
            max_tokens,
            encoding: TokenEncoding::Cl100k,
            overflow_model: None,
            split: None,
            map_reduce: None,
            visible_from: None,
            hidden: HashSet::new(),
            masked: HashSet::new(),
            checkpoint_coverage: Vec::new(),
            todo: TodoContext::new(),
            correction_block: None,
            correction_block_tokens: 0,
            todo_restore_pending: false,
            summary_annotator: None,
            cached_items_tokens: 0,
            manual_compaction: false,
            last_tool_set: None,
        }
    }

    /// Build a manager from an ALREADY-RESOLVED window (or `None` when
    /// discovery failed). Pure — no I/O. A REAL discovered window is resized
    /// to its EFFECTIVE value — the "sweet spot" budget that sits below the
    /// model's degradation zone (see [`effective_context_window`]); the
    /// fallback stays [`MAX_CONTEXT_TOKENS`] because the default is not a
    /// real window. [`Self::with_discovered_context`] resolves the window
    /// through discovery and delegates here.
    pub fn from_window(discovered: Option<usize>) -> Self {
        let max_tokens = discovered
            // Only a REAL discovered window is resized to its effective value;
            // the default fallback stays untouched (it is not a real window).
            .map(effective_context_window)
            .unwrap_or(MAX_CONTEXT_TOKENS);
        Self::new(max_tokens)
    }

    /// Create a new ContextManager with automatic context window discovery.
    ///
    /// Attempts to discover the model's context window via public APIs (OpenRouter
    /// and Anthropic) and delegates to [`Self::from_window`].
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
        Self::from_window(discover_context_window(model_name, Some("cosh/cache")).await)
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

    /// Record a context window reported by a provider context-window overflow.
    ///
    /// A provider's overflow is its OWN report of the maximum input size, so it
    /// is treated exactly like a successful discovery: the budget is re-sized
    /// to the EFFECTIVE value ([`effective_context_window`]), replacing the 100k
    /// fallback, and the RAW window is persisted to the provider-error catalog
    /// so future launches discover it before ever paying another overflow.
    ///
    /// Persistence is skipped under `cfg(test)`: tests run against the real
    /// `~/.local/share/cosh` data dir and must never write to it.
    pub fn record_provider_window(&mut self, model: Option<&str>, window: usize) {
        self.set_max_tokens(effective_context_window(window));
        let Some(model) = model else {
            return;
        };
        if cfg!(test) {
            return;
        }
        save_error_catalog_window(model, window);
    }

    /// Replace the mirrored tool TODO list. The harness syncs it from the
    /// tools' `Plan` state at loop start and after every `plan_*` dispatch.
    /// The dedicated [`TodoContext`] renders it as a protected block at the
    /// END of the messages — after the history, merged into a trailing `user`
    /// turn — structurally immune to every compaction phase and placed at the
    /// tail so a re-render never invalidates the provider's prefix cache (see
    /// [`todo_ctxt`] for the removal rules: all tasks terminal, or the model
    /// empties the plan).
    pub fn set_todo_list(&mut self, list: TodoList) {
        self.todo.sync(list);
    }

    /// The protected plan projection, including empty or completed plans.
    pub fn todo_list(&self) -> Option<&TodoList> {
        self.todo.list()
    }

    /// Consume a restore request, including an explicit reset for absent plans.
    pub(crate) fn take_restored_todo_list(&mut self) -> Option<TodoList> {
        std::mem::take(&mut self.todo_restore_pending)
            .then(|| self.todo.list().cloned().unwrap_or_default())
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

    /// Record a terminal API/runtime error as a DISPLAY-ONLY timeline item:
    /// skipped by [`Self::build_messages`] (the model never sees a provider
    /// failure as assistant output) and by the summarizers, and worth zero
    /// tokens. Persisted so the transcript restores the styled error line.
    pub fn add_error(&mut self, text: &str) {
        let id = self.next_id();
        self.push_item(ContextItem::Error {
            id,
            content: text.to_string(),
        });
    }

    /// Record a USER-TYPED shell command (Command mode) as a HIDDEN
    /// display-only timeline item: persisted to the session JSONL for the
    /// user, but immediately registered in the `hidden` set so
    /// [`Self::build_messages`], the token budget and every summarizer skip
    /// it — the model never sees it. Returns the item id.
    pub fn add_hidden_user_command(&mut self, text: &str) -> u64 {
        let id = self.next_id();
        self.push_item(ContextItem::UserCommand {
            id,
            content: text.to_string(),
        });
        // hide_at (not a bare hidden.insert): it also drops the item's cost
        // from the cached token total, keeping the debug drift check honest.
        self.hide_at(self.items.len() - 1);
        id
    }

    /// Record the RESULT of a user-typed Command-mode execution as a HIDDEN
    /// `bash_run` tool-call/tool-result pair: persisted to the session JSONL
    /// (the transcript shows what the "terminal" ran and produced), but both
    /// ids are immediately registered in the `hidden` set so the model never
    /// sees them. Returns `(call_id, result_id)`.
    pub fn add_hidden_command_execution(
        &mut self,
        call_id: &str,
        command: &str,
        result: &str,
    ) -> (u64, u64) {
        let call = self.next_id();
        self.push_item(ContextItem::ToolCall {
            id: call,
            call_id: call_id.to_string(),
            name: "bash_run".to_string(),
            arguments: serde_json::json!({ "command": command }).to_string(),
            thought_signature: String::new(),
            thinking_blocks: Vec::new(),
        });
        let result_id = self.next_id();
        self.push_item(ContextItem::ToolResult {
            id: result_id,
            call_id: call_id.to_string(),
            content: result.to_string(),
            useless: false,
        });
        // hide_at (not a bare hidden.insert): it also drops each item's cost
        // from the cached token total, keeping the debug drift check honest.
        let call_idx = self.items.len() - 2;
        let result_idx = self.items.len() - 1;
        self.hide_at(call_idx);
        self.hide_at(result_idx);
        (call, result_id)
    }

    /// Add a tool CALL. Structural, never prose-compressed. Renders as an
    /// `assistant` message with native `tool_calls` and no provider-specific
    /// replay metadata (the empty-signature path).
    pub fn add_tool_call(&mut self, call_id: &str, name: &str, arguments: &str) {
        self.add_tool_call_with_thinking(call_id, name, arguments, "", Vec::new());
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
            self.items
                .iter()
                .rev()
                .find(|item| {
                    !self.is_hidden(item)
                        && !matches!(
                            item,
                            ContextItem::Compaction { .. } | ContextItem::Error { .. }
                        )
                }),
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
    /// The sweep also covers a PARTIAL abandoned run: a `ToolCall` with no
    /// matching `ToolResult` sitting between an older user turn and the new
    /// input is the debris of a run cancelled mid-execution. Such an orphan
    /// would be rejected by providers that validate the native
    /// `tool_call → tool` pairing, so it is removed too — which then lets the
    /// older user turn behind it be recognized as abandoned. A completed
    /// chain (call WITH result) is never touched, and an orphan at the very
    /// END of the timeline (a call whose result is still pending) is never
    /// touched — only orphans followed by a new user turn.
    ///
    /// Called by the harness at loop start right after the fresh input is
    /// added, so an abandoned prompt never survives into the request.
    ///
    /// Returns whether at least one abandoned item was removed.
    pub fn remove_abandoned_inputs(&mut self) -> bool {
        let mut removed = false;
        loop {
            if self.drop_orphan_calls_before_trailing_users() {
                removed = true;
                continue;
            }
            if self.drop_abandoned_user_runs() {
                removed = true;
                continue;
            }
            break;
        }
        removed
    }

    /// Hide orphan `ToolCall`s in the tool-item zone that sits between the
    /// trailing run of user turns and the older history: a call without its
    /// result, followed by NEW input, is debris from a cancelled run (a
    /// pending call of a live run is never followed by a user turn). The zone
    /// is scanned back to front until the first non-tool item; completed
    /// chains inside it (call WITH result — including mixed parallel runs
    /// where only some results arrived) are preserved whole. Append-only:
    /// the debris STAYS in the timeline, hidden from the model. Returns
    /// whether anything was hidden.
    fn drop_orphan_calls_before_trailing_users(&mut self) -> bool {
        // Everything below works on VISIBLE items only: a hidden item is "not
        // there" for both the model and this scan — treating it otherwise
        // would re-hide the same debris forever (the caller loops until
        // quiescence). Indexes are the raw timeline positions.
        let mut trailing_users = 0usize;
        let mut users_start = self.items.len();
        for (idx, item) in self.items.iter().enumerate().rev() {
            if self.is_hidden(item) {
                continue;
            }
            if matches!(item, ContextItem::User { .. }) {
                trailing_users += 1;
                users_start = idx;
            } else {
                break;
            }
        }
        if trailing_users == 0 {
            return false;
        }
        // All `call_id`s that have a VISIBLE result in the timeline — a call
        // holding one of these is a completed chain, never orphan debris.
        let paired: HashSet<String> = self
            .items
            .iter()
            .filter(|it| !self.is_hidden(it))
            .filter_map(|it| match it {
                ContextItem::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let mut hidden_any = false;
        let mut idx = users_start;
        while idx > 0 {
            idx -= 1;
            if self.is_hidden(&self.items[idx]) {
                // Hidden debris from an earlier pass — not part of the zone.
                continue;
            }
            let doomed = match &self.items[idx] {
                ContextItem::ToolCall { call_id, .. } => !paired.contains(call_id),
                // A tool result (of a preserved call) does not end the zone:
                // an orphaned PARALLEL call can sit next to a completed one.
                ContextItem::ToolResult { .. } => continue,
                _ => break,
            };
            if doomed {
                self.hide_at(idx);
                hidden_any = true;
            }
        }
        hidden_any
    }

    /// Hide every trailing `User` turn but the newest (the abandoned-input
    /// rule) — append-only: they stay in the timeline, never shown to the
    /// model. Returns whether anything was hidden.
    fn drop_abandoned_user_runs(&mut self) -> bool {
        // Count the trailing consecutive VISIBLE User turns (a hidden one is
        // "not there" — skipping it, not breaking, so an already-hidden run
        // below the visible frontier never trips the quiescence loop).
        let mut user_indices: Vec<usize> = Vec::new();
        for (idx, item) in self.items.iter().enumerate().rev() {
            if self.is_hidden(item) {
                continue;
            }
            if matches!(item, ContextItem::User { .. }) {
                user_indices.push(idx);
            } else {
                break;
            }
        }
        if user_indices.len() <= 1 {
            return false;
        }
        // Keep the NEWEST input (the last visible item); hide the rest of
        // the run.
        for &idx in &user_indices[1..] {
            self.hide_at(idx);
        }
        true
    }

    /// Promote the last assistant text output of the finished agent loop into a
    /// [`ContextItem::Closure`], replacing the raw text in place. Guard: if
    /// the last item is not an assistant text (e.g. it was a tool call), it
    /// cannot be a summary of what was done, so no `Closure` is created.
    ///
    /// The harness has now told us this turn was the loop's FINAL output: the
    /// ORIGINAL text is promoted verbatim, so it is rendered verbatim for as
    /// long as it stays in the timeline. (A later compaction still folds it
    /// into the summary along with everything else.)
    pub fn close_loop(&mut self) {
        let Some(ContextItem::Assistant {
            id,
            original,
            closable,
        }) = self.items.back()
        else {
            return;
        };
        if !*closable {
            return;
        }
        let draft_id = *id;
        let content = original.clone();
        // Swap the draft in place for a Closure (SAME id, immediate). This is
        // a transform, not a hide: the item's identity and visibility are
        // unchanged, so the token cache is adjusted by the exact delta.
        let last = self.items.len() - 1;
        let old_tokens = self.items[last].tokens(self.encoding);
        self.items[last] = ContextItem::Closure {
            id: draft_id,
            content,
        };
        let new_tokens = self.items[last].tokens(self.encoding);
        self.cached_items_tokens = self
            .cached_items_tokens
            .saturating_sub(old_tokens)
            .saturating_add(new_tokens);
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
    /// There is deliberately NO deterministic masking here: a mutation in the
    /// middle of the history invalidates the exact-prefix cache of every
    /// provider for everything after the changed position. Budget relief near
    /// the trigger comes from the model's explicit `mask_tool_result` calls
    /// (tail-only, cache-neutral) or from the LLM compaction, never from a
    /// mid-history mutation.
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

    /// Hide every tool chain whose result is marked `useless`. Runs at the
    /// start of every [`Self::run`], regardless of the budget. The newest
    /// chain is preserved — the model has not reacted to it yet — EXCEPT when
    /// its result was explicitly masked by a model `mask_tool_result` call:
    /// that call IS the reaction, so it is the one case where the newest
    /// chain may be hidden. The chains
    /// STAY in the timeline (append-only) — only their visibility flips, so
    /// revert/fork keep reaching them.
    fn sweep_useless_chains(&mut self) {
        // The newest chain (the one the model has not seen yet) is preserved:
        // its CALL id is the chain identity, so BOTH halves are excluded.
        // Visibility-aware throughout: a chain already hidden by an earlier
        // pass is inert (re-hiding it would be a no-op, but scanning it could
        // wrongly anchor `newest_call_id`).
        let newest_call_id: Option<String> = self
            .items
            .iter()
            .rev()
            .filter(|it| !self.is_hidden(it))
            .find_map(|it| it.call_id().map(str::to_string));
        let useless_call_ids: HashSet<String> = self
            .items
            .iter()
            .filter(|it| !self.is_hidden(it))
            .filter_map(|it| match it {
                ContextItem::ToolResult {
                    call_id,
                    useless: true,
                    ..
                } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        // Explicitly masked results (model `mask_tool_result` calls) mapped
        // to their chain identity: masking is the model's reaction to a
        // result, so the newest-chain protection no longer applies to them.
        let masked_call_ids: HashSet<String> = self
            .items
            .iter()
            .filter(|it| !self.is_hidden(it))
            .filter_map(|it| match it {
                ContextItem::ToolResult { id, call_id, .. } if self.masked.contains(id) => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .collect();
        // Back-to-front so hiding a chain never invalidates a pending index;
        // hide_item also hides the partner half, so its index is skipped
        // naturally by the next iteration. Idempotent: an already-hidden
        // chain lands in the set again (a no-op).
        let mut idx = self.items.len();
        while idx > 0 {
            idx -= 1;
            if self.is_hidden(&self.items[idx]) {
                continue;
            }
            let doomed = match &self.items[idx] {
                ContextItem::ToolCall { call_id, .. } | ContextItem::ToolResult { call_id, .. } => {
                    useless_call_ids.contains(call_id)
                        && (newest_call_id.as_deref() != Some(call_id.as_str())
                            || masked_call_ids.contains(call_id))
                }
                _ => false,
            };
            if doomed {
                self.hide_item(idx);
            }
        }
    }

    /// Model-driven tail masking (the harness `mask_tool_result` tool):
    /// replace the NEWEST visible tool-result payload with a small typed
    /// reference in the model-facing projection. The raw result stays in
    /// `items` and in immutable history, and remains available to checkpoint
    /// construction, so a future rehydration tool can read it back.
    ///
    /// Only the tail is ever touched: a mutation at the end of the timeline
    /// cannot invalidate a cached prefix (the tail is new, uncached content
    /// anyway), which is why the retired deterministic mid-history masking
    /// was replaced by this explicit model request. Idempotent: an already
    /// masked or missing result yields `None`.
    ///
    /// Returns the masked source item id.
    pub fn mask_newest_tool_result(&mut self) -> Option<u64> {
        // The mask tool's own confirmation results are never targets: a
        // reflex call must mask a REAL tool result, not the feedback of a
        // previous mask call (its chain is swept as useless anyway).
        let mask_call_ids: HashSet<&str> = self
            .items
            .iter()
            .filter_map(|it| match it {
                ContextItem::ToolCall { call_id, name, .. } if name == "mask_tool_result" => {
                    Some(call_id.as_str())
                }
                _ => None,
            })
            .collect();
        let idx = self
            .items
            .iter()
            .enumerate()
            .rev()
            .find(|(_, item)| {
                !self.is_hidden(item)
                    && match item {
                        ContextItem::ToolResult { id, call_id, .. } => {
                            !self.masked.contains(id) && !mask_call_ids.contains(call_id.as_str())
                        }
                        _ => false,
                    }
            })
            .map(|(idx, _)| idx)?;
        let id = match &self.items[idx] {
            ContextItem::ToolResult { id, .. } => *id,
            _ => unreachable!("filtered above"),
        };
        let old_tokens = self.visible_item_tokens(&self.items[idx]);
        let new_tokens = self.encoding.estimate(&masked_tool_result(id));
        self.masked.insert(id);
        self.cached_items_tokens = self
            .cached_items_tokens
            .saturating_sub(old_tokens)
            .saturating_add(new_tokens);
        Some(id)
    }

    /// Token-bounded start of the high-fidelity recent window.
    /// The window is token-bounded and expanded backwards when its first tool
    /// result needs an earlier matching call to keep native tool structure
    /// valid.
    fn recent_raw_start_for(&self, max_tokens: usize) -> usize {
        let target = max_tokens.saturating_mul(RECENT_RAW_PCT) / 100;
        let mut held = 0usize;
        let mut start = self.items.len();
        for (idx, item) in self.items.iter().enumerate().rev() {
            if self.is_hidden(item) || matches!(item, ContextItem::Compaction { .. }) {
                continue;
            }
            let tokens = self.visible_item_tokens(item);
            if start < self.items.len() && (held >= target || held.saturating_add(tokens) > target)
            {
                break;
            }
            held = held.saturating_add(tokens);
            start = idx;
        }
        if start == self.items.len() {
            return start;
        }

        // A result in the retained tail must keep its native call. Parallel
        // calls can put several results after the boundary, so expand to the
        // oldest required call in one pass.
        let retained_results: HashSet<String> = self
            .items
            .iter()
            .skip(start)
            .filter(|item| !self.is_hidden(item))
            .filter_map(|item| match item {
                ContextItem::ToolResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        for (idx, item) in self.items.iter().take(start).enumerate() {
            if let ContextItem::ToolCall { call_id, .. } = item
                && retained_results.contains(call_id)
            {
                start = start.min(idx);
            }
        }
        start
    }

    /// Raw indexes selected for the next checkpoint. Automatic compaction
    /// covers the old prefix and every previous checkpoint while preserving a
    /// recent raw tail. Manual compaction intentionally keeps its historical
    /// whole-view behavior. A single item larger than the budget is selected
    /// whole because no non-empty tail/source split exists.
    fn checkpoint_source_indices(&self) -> Vec<usize> {
        self.checkpoint_source_indices_for(self.max_tokens)
    }

    fn checkpoint_source_indices_for(&self, max_tokens: usize) -> Vec<usize> {
        let mut visible = self.all_visible_source_indices();
        // A checkpoint is the base state, even though it was appended after
        // the raw tail it precedes in the model view.
        visible.sort_by_key(|&idx| {
            (
                !matches!(self.items[idx], ContextItem::Compaction { .. }),
                idx,
            )
        });
        // Inputs awaiting the next model response remain verbatim, including
        // queued steering that arrived after the most recent tool result.
        let pending_inputs: HashSet<u64> = self
            .items
            .iter()
            .rev()
            .filter(|item| {
                !self.is_hidden(item)
                    && !matches!(
                        item,
                        ContextItem::Compaction { .. } | ContextItem::Error { .. }
                    )
            })
            .take_while(|item| matches!(item, ContextItem::User { .. }))
            .map(ContextItem::id)
            .collect();
        let has_prior_work = visible
            .iter()
            .any(|&idx| !matches!(self.items[idx], ContextItem::User { .. }));
        if has_prior_work {
            visible.retain(|&idx| !pending_inputs.contains(&self.items[idx].id()));
        }
        if self.manual_compaction {
            return visible;
        }
        let raw_start = self.recent_raw_start_for(max_tokens);
        let mut source: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&idx| {
                idx < raw_start || matches!(self.items[idx], ContextItem::Compaction { .. })
            })
            .collect();
        let source_set: HashSet<usize> = source.iter().copied().collect();
        let raw_tokens = visible
            .iter()
            .filter(|idx| !source_set.contains(idx))
            .fold(0usize, |total, &idx| {
                total.saturating_add(self.visible_item_tokens(&self.items[idx]))
            });
        let raw_target = max_tokens.saturating_mul(RECENT_RAW_PCT) / 100;
        if raw_tokens > raw_target {
            // Pair-preserving expansion (or one giant newest item) can make
            // the supposedly recent tail larger than its entire reservation.
            // Fold it whole rather than mutate provider-signed tool calls or
            // commit a checkpoint that cannot relieve pressure.
            return visible;
        }
        if source.is_empty() && self.total_tokens() >= max_tokens.saturating_mul(COMPACT_PCT) / 100
        {
            // Pathological one-item context: preserving a raw tail would leave
            // nothing to checkpoint and cannot relieve the overflow.
            source = std::mem::take(&mut visible);
        }
        source
    }

    fn all_visible_source_indices(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| !self.is_hidden(item) && !matches!(item, ContextItem::Error { .. }))
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Build an LLM-checkpoint request from the selected old prefix, serialized
    /// into an opencode-style transcript. The recent raw window is deliberately
    /// excluded and remains verbatim after the checkpoint is committed.
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
        let source = self.checkpoint_source_indices();
        if source.is_empty() {
            return None;
        }
        // Only the selected OLD prefix is checkpointed. The most recent turn
        // stays raw and is composed after the checkpoint on the next model
        // request. Masked results deliberately serialize their original
        // payload here: masking is a view optimization, never data loss.
        let has_previous_summary = source
            .iter()
            .any(|&idx| matches!(self.items[idx], ContextItem::Compaction { .. }));
        let context = source
            .iter()
            .map(|&idx| &self.items[idx])
            .map(|item| format!("[Context item #{}]\n{}", item.id(), serialize_item(item)))
            .filter(|line| !line.is_empty())
            .collect::<Vec<String>>()
            .join("\n\n");
        Some(LlmCompactionRequest {
            system: SUMMARIZER_SYSTEM.to_string(),
            prompt: build_llm_prompt(has_previous_summary, &context),
        })
    }

    /// Atomically commit an LLM checkpoint over the planned old prefix. Exact
    /// coverage is stored on the appended item and the recent raw tail remains
    /// visible. A candidate that would not land below the normal 80% trigger is
    /// rejected without changing the view. Manual `/compact` selects the full
    /// visible source to preserve its explicit whole-session semantics.
    pub fn apply_llm_summary(&mut self, summary: String) -> bool {
        let mut source = self.checkpoint_source_indices();
        if source.is_empty() {
            // Direct callers can apply a summary below the automatic trigger.
            // With no planned old prefix, preserve the historical whole-view
            // behavior rather than silently accepting an empty checkpoint.
            source = self.all_visible_source_indices();
        }
        self.apply_summary_to_source(summary, &source)
    }

    /// Apply a summary explicitly produced from the complete visible source.
    /// This keeps the legacy split staging correct until MapReduce replaces it:
    /// the split already summarized the raw tail, so retaining that tail would
    /// duplicate it in the model view.
    pub(super) fn apply_full_llm_summary(&mut self, summary: String, window: usize) -> bool {
        let source = self.all_visible_source_indices();
        self.apply_summary_to_source_with_trigger(summary, &source, window.saturating_add(1))
    }

    fn apply_summary_to_source(&mut self, summary: String, source: &[usize]) -> bool {
        self.apply_summary_to_source_with_trigger(summary, source, self.normal_trigger())
    }

    fn apply_summary_to_source_with_trigger(
        &mut self,
        summary: String,
        source: &[usize],
        trigger: usize,
    ) -> bool {
        if source.is_empty() || summary.trim().is_empty() {
            return false;
        }
        // Validate the exact text that will be installed. The annotation can
        // grow or shrink a handoff; checking the unannotated candidate first
        // would allow an over-budget commit (or reject a fitting one).
        let summary = match &self.summary_annotator {
            Some(annotate) => annotate(&summary),
            None => summary,
        };
        if summary.trim().is_empty() {
            return false;
        }
        let removed_tokens = source.iter().fold(0usize, |total, &idx| {
            total.saturating_add(self.visible_item_tokens(&self.items[idx]))
        });
        let projected = self
            .cached_items_tokens
            .saturating_sub(removed_tokens)
            .saturating_add(self.encoding.estimate(&summary))
            .saturating_add(self.todo.tokens(self.encoding));
        if projected >= trigger {
            // A degenerate summary is not a state transition. Keep the current
            // model view intact so failure and retry are genuinely atomic.
            return false;
        }
        self.commit_summary_to_source(summary, source);
        self.clear_overflow();
        true
    }

    /// Install the summary annotator used by every compaction commit path.
    /// See the field docs for the ownership split (harness owns the tool
    /// set; the manager owns the single commit funnel).
    pub fn set_summary_annotator(&mut self, annotator: Box<dyn Fn(&str) -> String + Send + Sync>) {
        self.summary_annotator = Some(annotator);
    }

    fn commit_summary_to_source(&mut self, summary: String, source: &[usize]) {
        let direct_ids: Vec<u64> = source.iter().map(|&idx| self.items[idx].id()).collect();
        let mut all_ranges = coalesce_ranges(&direct_ids);
        for &idx in source {
            if let ContextItem::Compaction {
                covered_ranges: prior_ranges,
                ..
            } = &self.items[idx]
            {
                all_ranges.extend_from_slice(prior_ranges);
            }
        }
        let covered_ranges = merge_ranges(all_ranges);
        let id = self.next_id();
        // Commit atomically in the runtime projection: cover only the planned
        // old prefix. The recent raw tail remains visible even though its IDs
        // precede the newly appended checkpoint; coverage comes from the
        // checkpoint ranges instead of advancing the legacy prefix boundary.
        for covered in &direct_ids {
            self.masked.remove(covered);
        }
        self.push_item(ContextItem::Compaction {
            id,
            summary,
            covered_ranges: covered_ranges.clone(),
        });
        self.checkpoint_coverage = covered_ranges;
        // Coverage is derived from the newly appended checkpoint itself. This
        // makes revert/fork removal automatically reveal its source without a
        // compensating visibility mutation.
        self.recompute_cached_tokens();
    }

    /// True when the LLM compaction is KNOWN to be stuck for `model`: a
    /// previous overflow could not be relieved by the split and the total
    /// still exceeds the window. The harness then skips the doomed summarizer
    /// call (it would only burn a paid request per dispatch) and re-surfaces
    /// the notification until the user switches the model. Keyed by MODEL —
    /// the stuck constraint is the model's window, not the provider's API.
    pub fn overflow_stuck(&self, model: &str) -> bool {
        self.overflow_model.as_deref() == Some(model)
    }

    /// Record that the LLM compaction is stuck for `model` (the split could
    /// not fit the context into the model window).
    pub fn mark_overflow(&mut self, model: &str) {
        self.overflow_model = Some(model.to_string());
    }

    /// The current token budget — the discovered window or the default. The
    /// harness uses it (with the error-reported window) to detect the
    /// context-exceeds-window split trigger.
    #[must_use]
    pub const fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Forget a recorded stuck overflow — called when the model changes
    /// (model switch) or a compaction succeeds, so the next overflow starts
    /// fresh.
    fn clear_overflow(&mut self) {
        self.overflow_model = None;
    }

    /// Called by the harness at loop start with the ACTIVE model: when it
    /// differs from a recorded stuck model, the user switched the model —
    /// clear the stuck state and retry from scratch.
    pub fn sync_model(&mut self, model: &str) {
        if self.overflow_model.as_deref() != Some(model) {
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
    pub fn build_messages(&self, current_input: &str) -> Vec<ChatMessage> {
        let mut messages = Vec::with_capacity(self.items.len() + 1);
        // Visibility filter FIRST (O(1) per item — boundary compare + hash
        // lookup): pre-compaction and individually hidden items never reach
        // the model. Then skip leading tool results — a `tool` message right
        // after `system` is rejected by some providers (e.g. Mistral).
        // Orphans should not exist thanks to chain-aware hiding, but a
        // restored snapshot could still hold them.
        let visible: Vec<&ContextItem> =
            self.items.iter().filter(|it| !self.is_hidden(it)).collect();
        // Checkpoints are append-only timeline items, but semantically form
        // the handoff prefix for the raw recent window. Compose them first;
        // retain chronological order within checkpoints and within raw items.
        let projected = visible
            .iter()
            .copied()
            .filter(|it| matches!(it, ContextItem::Compaction { .. }))
            .chain(
                visible
                    .iter()
                    .copied()
                    .filter(|it| !matches!(it, ContextItem::Compaction { .. })),
            )
            .skip_while(|it| matches!(it, ContextItem::ToolResult { .. }));
        for item in projected {
            match item {
                ContextItem::User { original, .. } => {
                    // User prompts are rendered verbatim.
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
                    id,
                    call_id,
                    content,
                    ..
                } => {
                    let rendered = if self.masked.contains(id) {
                        masked_tool_result(*id)
                    } else {
                        content.clone()
                    };
                    messages.push(tool_result_message(call_id, &rendered));
                }
                ContextItem::Closure { content, .. } => {
                    messages.push(assistant_message(content));
                }
                ContextItem::Compaction { summary, .. } => {
                    messages.push(assistant_message(summary));
                }
                // Display-only error line: never part of the model-facing
                // conversation.
                ContextItem::Error { .. } => {}
                // User-typed Command-mode command: display-only, never part
                // of the model-facing conversation.
                ContextItem::UserCommand { .. } => {}
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
        // Tail blocks in order: the tool-correction block first, then the
        // protected TODO block. Corrections churn on every tool failure —
        // rendering them at the TAIL keeps that churn away from the cached
        // `[system → history]` prefix (a header placement would invalidate
        // the WHOLE conversation on every new correction, exactly like the
        // retired deterministic masking). A change here only costs the tail:
        // new, uncached content anyway. When the last message is itself a
        // `user` turn, the blocks are MERGED into it (blocks first, steering
        // after) instead of being injected as a second consecutive `user`
        // message (rejected by some providers).
        let mut tail_block: Option<String> = None;
        if let Some(corrections) = self.correction_block.as_deref() {
            if let Some(existing) = tail_block.as_mut() {
                existing.push_str("\n\n");
                existing.push_str(corrections);
            } else {
                tail_block = Some(corrections.to_string());
            }
        }
        if let Some(block) = self.todo.text() {
            if let Some(existing) = tail_block.as_mut() {
                existing.push_str("\n\n");
                existing.push_str(&block);
            } else {
                tail_block = Some(block);
            }
        }
        if let Some(block) = tail_block {
            match messages.last_mut() {
                Some(last) if last.role == "user" => {
                    let existing = last.content.take().unwrap_or_default();
                    let mut combined = block;
                    if !existing.is_empty() {
                        combined.push_str("\n\n");
                        combined.push_str(&existing);
                    }
                    last.content = Some(combined);
                }
                _ => messages.push(user_message(&block)),
            }
        }
        messages
    }

    /// Transpile the CURRENT model-facing view of the session into a Markdown
    /// transcript. The projection mirrors [`Self::build_messages`] exactly:
    /// only items the agent can see right now (the `visible_from` boundary,
    /// the `hidden` set and checkpoint coverage all apply), checkpoints
    /// composed before the raw tail, masked tool results replaced by their
    /// typed reference. Display-only `Error` items never reach the model, so
    /// they are skipped here as well. The harness steering input and the
    /// TODO/correction tail blocks are per-request scaffolding, not
    /// transcript, and are left out.
    pub fn export_markdown(&self) -> String {
        let visible: Vec<&ContextItem> =
            self.items.iter().filter(|it| !self.is_hidden(it)).collect();
        let projected = visible
            .iter()
            .copied()
            .filter(|it| matches!(it, ContextItem::Compaction { .. }))
            .chain(
                visible
                    .iter()
                    .copied()
                    .filter(|it| !matches!(it, ContextItem::Compaction { .. })),
            )
            .skip_while(|it| matches!(it, ContextItem::ToolResult { .. }));
        let mut out = String::new();
        for item in projected {
            match item {
                ContextItem::User { original, .. } => {
                    push_section(&mut out, "## User", original);
                }
                ContextItem::Assistant { original, .. } => {
                    push_section(&mut out, "## Assistant", original);
                }
                ContextItem::Closure { content, .. } => {
                    push_section(&mut out, "## Assistant", content);
                }
                ContextItem::Compaction { summary, .. } => {
                    push_section(&mut out, "## Compaction checkpoint", summary);
                }
                ContextItem::ToolCall {
                    name, arguments, ..
                } => {
                    let mut body = String::new();
                    push_fenced(&mut body, "json", arguments);
                    push_section(&mut out, &format!("### Tool call: `{name}`"), &body);
                }
                ContextItem::ToolResult {
                    id,
                    content,
                    useless,
                    ..
                } => {
                    let rendered = if self.masked.contains(id) {
                        masked_tool_result(*id)
                    } else {
                        content.clone()
                    };
                    let heading = if *useless {
                        "### Tool result (no useful output)"
                    } else {
                        "### Tool result"
                    };
                    let mut body = String::new();
                    push_fenced(&mut body, "text", &rendered);
                    push_section(&mut out, heading, &body);
                }
                // Display-only error line: the model never sees it, so the
                // export of the agent's view skips it too.
                ContextItem::Error { .. } => {}
                // User-typed Command-mode command: display-only for the
                // model, so the export of the agent's view skips it too.
                ContextItem::UserCommand { .. } => {}
            }
        }
        out
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

    /// Serializable transport projection for persistence. The session store
    /// converts differences into immutable deltas; this clone is not a second
    /// source of truth.
    /// The effective tool set the model was last offered, if any. `None` on a
    /// brand-new session (no previous turn recorded one).
    pub fn last_tool_set(&self) -> Option<&[String]> {
        self.last_tool_set.as_deref()
    }

    /// Record the effective tool set the model is being offered this turn.
    pub fn set_last_tool_set(&mut self, mut tool_set: Vec<String>) {
        tool_set.sort_unstable();
        tool_set.dedup();
        self.last_tool_set = Some(tool_set);
    }

    /// Mirror the tool-correction memory block for the next request (tail
    /// rendering — see the field docs). An empty/blank text clears the block.
    pub fn set_correction_block(&mut self, text: String) {
        self.correction_block = if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
        self.correction_block_tokens = self
            .correction_block
            .as_deref()
            .map_or(0, |block| self.encoding.estimate(block));
    }

    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            items: self.items.clone(),
            next_id: self.next_id,
            max_tokens: self.max_tokens,
            overflow_model: self.overflow_model.clone(),
            split: self.split.clone(),
            map_reduce: self.map_reduce.clone(),
            visible_from: self.visible_from,
            hidden: self.hidden.clone(),
            masked: self.masked.clone(),
            todo: self.todo.list().cloned(),
            last_tool_set: self.last_tool_set.clone(),
        }
    }

    /// Restore a previously saved snapshot. The items are restored verbatim;
    /// the next 80% overflow triggers the LLM compaction exactly as in a fresh
    /// session.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.todo_restore_pending = true;
        self.items = state.items.clone();
        self.next_id = state.next_id;
        self.max_tokens = state.max_tokens;
        self.overflow_model = state.overflow_model.clone();
        // Split progress is REAL progress (the buffer + cursor) — a restored
        // session resumes the split exactly where it stopped.
        self.split = state.split.clone();
        self.map_reduce = state.map_reduce.clone();
        self.visible_from = state.visible_from;
        self.hidden = state.hidden.clone();
        self.masked = state.masked.clone();
        let live_results: HashSet<u64> = self
            .items
            .iter()
            .filter_map(|item| match item {
                ContextItem::ToolResult { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        self.masked.retain(|id| live_results.contains(id));
        self.rebuild_checkpoint_coverage();
        match &state.todo {
            Some(list) => self.todo.sync(list.clone()),
            None => self.todo.clear(),
        }
        self.last_tool_set = state.last_tool_set.clone();
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
        if let Some(tx) = &self.binding_tx {
            let user = matches!(item, ContextItem::User { .. });
            // Errors render only after their terminal event; that consumer
            // binds the included error snapshot after creating the message.
            if !matches!(item, ContextItem::Error { .. }) {
                let call_item_id = match &item {
                    ContextItem::ToolResult { call_id, .. } => {
                        self.items.iter().rev().find_map(|i| match i {
                            ContextItem::ToolCall {
                                id, call_id: owner, ..
                            } if owner == call_id => Some(*id),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                let tool_name = match &item {
                    ContextItem::ToolCall { name, .. } => Some(name.clone()),
                    _ => None,
                };
                let compaction = matches!(item, ContextItem::Compaction { .. });
                let _ = tx.send(super::events::HarnessEvent::ContextItemRecorded {
                    item_id: item.id(),
                    user,
                    call_item_id,
                    tool_name,
                    compaction,
                });
            }
        }
        let tokens = item.tokens(self.encoding);
        self.cached_items_tokens = self.cached_items_tokens.saturating_add(tokens);
        self.items.push_back(item);
    }

    pub(crate) fn set_binding_sender(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        self.binding_tx = Some(tx);
    }

    /// Rebuild the cached token total from scratch. Called when the encoding
    /// changes (every item's estimated cost changes), on restore and on
    /// compaction. Counts ONLY model-visible items — the cache represents
    /// what the provider would be sent, never the hidden append-only history.
    fn recompute_cached_tokens(&mut self) {
        self.cached_items_tokens = self
            .items
            .iter()
            .filter(|it| !self.is_hidden(it))
            .fold(0usize, |acc, it| {
                acc.saturating_add(self.visible_item_tokens(it))
            });
    }

    fn visible_item_tokens(&self, item: &ContextItem) -> usize {
        match item {
            ContextItem::ToolResult { id, .. } if self.masked.contains(id) => {
                self.encoding.estimate(&masked_tool_result(*id))
            }
            _ => item.tokens(self.encoding),
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

    /// True when there is anything worth compacting: at least one VISIBLE
    /// item that is not itself the (newest) compaction summary. Append-only
    /// means the timeline grows forever — the check is on visibility, never
    /// on the raw length.
    pub fn has_compactable_content(&self) -> bool {
        self.items
            .iter()
            .any(|it| !self.is_hidden(it) && !matches!(it, ContextItem::Compaction { .. }))
    }

    fn total_tokens(&self) -> usize {
        // Debug-only invariant: the incremental cache has a handful of sync
        // points, so a future mutation that forgets to update it would
        // silently corrupt compaction timing (saturating arithmetic masks
        // underflow). Every call to `total_tokens` — which the whole test
        // suite makes constantly — cross-checks the cached total against a
        // brute-force recompute in debug builds, failing fast on drift.
        // Compiled out in release: zero cost.
        #[cfg(debug_assertions)]
        {
            let brute: usize = self
                .items
                .iter()
                .filter(|it| !self.is_hidden(it))
                .map(|it| self.visible_item_tokens(it))
                .sum();
            debug_assert_eq!(
                self.cached_items_tokens, brute,
                "cached token total drifted from items ({}, expected {})",
                self.cached_items_tokens, brute
            );
        }
        self.cached_items_tokens
            .saturating_add(self.todo.tokens(self.encoding))
            .saturating_add(self.correction_block_tokens)
    }

    /// Whether the model must not see this raw item: it is behind a legacy
    /// boundary, explicitly hidden as debris, or covered by the newest derived
    /// checkpoint. Checkpoint coverage is intentionally derived so removing an
    /// anchor through revert/fork automatically reveals its source.
    fn is_hidden(&self, item: &ContextItem) -> bool {
        let id = item.id();
        id < self.visible_from.unwrap_or(0)
            || self.hidden.contains(&id)
            || self.latest_checkpoint_covers(id)
    }

    fn latest_checkpoint_covers(&self, item_id: u64) -> bool {
        self.checkpoint_coverage
            .iter()
            .any(|range| range.start_id <= item_id && item_id <= range.end_id)
    }

    fn rebuild_checkpoint_coverage(&mut self) {
        self.checkpoint_coverage = self
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                ContextItem::Compaction {
                    id, covered_ranges, ..
                } if *id >= self.visible_from.unwrap_or(0) && !self.hidden.contains(id) => {
                    Some(covered_ranges.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
    }

    /// Hide the item at `idx`: it stays in the timeline (revert/fork can
    /// still reach it) but is never sent to the model. The item's cost
    /// leaves the cached token total — the doomed item is re-tokenized here
    /// (hiding is rare and bounded, so this is the cheap side of not keeping
    /// a per-item cost map in sync).
    fn hide_at(&mut self, idx: usize) {
        let id = self.items[idx].id();
        let tokens = self.visible_item_tokens(&self.items[idx]);
        let checkpoint = matches!(self.items[idx], ContextItem::Compaction { .. });
        self.hidden.insert(id);
        self.masked.remove(&id);
        self.cached_items_tokens = self.cached_items_tokens.saturating_sub(tokens);
        if checkpoint {
            self.rebuild_checkpoint_coverage();
            self.recompute_cached_tokens();
        }
    }

    /// Hide the item at `idx`. For tool items the matching call/result
    /// partner is hidden too, so the native `tool_call → tool` chain can
    /// never break (a lone `tool` message or an orphaned tool call is
    /// rejected by providers).
    fn hide_item(&mut self, idx: usize) {
        let is_call = matches!(self.items[idx], ContextItem::ToolCall { .. });
        let Some(cid) = self.items[idx].call_id().map(str::to_string) else {
            self.hide_at(idx);
            return;
        };
        let partner = self.items.iter().position(|it| match it {
            ContextItem::ToolCall { call_id: c, .. } if !is_call => *c == cid,
            ContextItem::ToolResult { call_id: c, .. } if is_call => *c == cid,
            _ => false,
        });
        match partner {
            // Hide both halves so the native pairing never breaks.
            Some(p) => {
                self.hide_at(idx.max(p));
                self.hide_at(idx.min(p));
            }
            // No partner (orphan) — hide just this item.
            None => self.hide_at(idx),
        }
    }
}
