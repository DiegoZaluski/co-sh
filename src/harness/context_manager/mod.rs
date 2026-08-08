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
//! When the held context reaches 80% of the budget, the compaction runs. A
//! **toggle** rotates which phase LEADS each overflow — pipeline → draft
//! eviction → tool eviction, back to the pipeline — and within a single
//! overflow the funnel falls through the toggle cycle (wrapping to the
//! pipeline if needed) until the total drops below the 80% trigger:
//!
//!   1. **The pipeline** (`pipeline_pass`) — the compression runs here,
//!      synchronously, on the agent loop's thread. It walks the timeline
//!      oldest-first and summarizes every unprotected draft (old user
//!      prompts + compressible assistant outputs), **segment by segment**: the
//!      budget is checked at each [`LoopClosure`] boundary, so one completed
//!      loop's drafts are compressed as a coherent block before the next
//!      segment is considered. Below the trigger at a boundary → the pass is
//!      done; still over → it continues into the next segment. It leads the
//!      first overflow of each cycle and hands the lead to phase 2 when it
//!      resolves;
//!   2. **Gradual draft eviction** (`evict_drafts`) — the same draft pass as
//!      before (old user inputs + assistant outputs, oldest-first, never
//!      touching [`LoopClosure`]s, protected prompts, the [`PromptAnchor`] or
//!      tool chains) but reduced to ONE chunk per stop: it removes a single
//!      whole input/output, checks the budget, and stops the moment the total
//!      drops below 80%. It is the toggle's **sticky** lead: it stays across
//!      consecutive overflows (the persistent cursor resumes where it stopped,
//!      so the context lives one overflow at a time) and only hands the lead
//!      to phase 3 once the cursor reaches a [`LoopClosure`] (or exhausts the
//!      timeline still over the trigger) — the oldest segment's drafts are
//!      exhausted. The [`LoopClosure`] is the shared CHECKPOINT of phases 1
//!      and 2: both stop at it and continue after it;
//!   3. **Tool eviction** (`evict_tools`) — tool chains are removed from the
//!      middle outward (the same idea as Goose's compaction fallback) until
//!      the total drops below the trigger, preserving the newest chain and
//!      the anchors. Completing it (as the lead or via the funnel) resets the
//!      toggle back to the pipeline — the cycle restarts;
//!   4. **LoopClosure trimming** (`trim_loop_closures`) — NOT part of the
//!      toggle: it is always evaluated last and fires on its own condition —
//!      when `LoopClosure`s alone hold ≥ 40% of the budget, the oldest one is
//!      discarded one at a time until the SUM of `LoopClosure`s drops below
//!      40% (the 40% limit applies only to the closures, never to the whole
//!      context).
//!
//! The [`PromptAnchor`] is the first user prompt of the session, or the first
//! prompt after a [`LoopClosure`] (a new task segment): it carries the user's
//! original intent. It is never submitted to the compression pipeline (stays
//! verbatim) and is protected from draft eviction while it is the anchor. It
//! is not eternal — when a new segment starts, the anchor moves to the new
//! prompt and the old one becomes an ordinary removable prompt.
//!
//! The two most recent user prompts are *protected* (never compressed, never
//! removed), and the first prompt of each task segment becomes a
//! [`PromptAnchor`] — verbatim and draft-eviction-proof while current.
//!
//! Two deliberate floors keep the total above the trigger in rare cases, by
//! design: (1) protected prompts and the current anchor are never removed, so
//! if those alone exceed the model window (or the closures are too small to
//! matter) the total can stay over budget — those anchors are intentionally
//! untouchable; (2) phase 2 stops at the first [`LoopClosure`] boundary, so a
//! pass can end over the trigger when the newest segment holds the only
//! removable mass and the tool/closure phases cannot relieve it — the next
//! `run()` leads with the tool pass and the funnel brings phase 2 past the
//! closure, converging. A huge user prompt needs no special casing: the
//! guards plus the fall-through handle it like any other overflow, trimming
//! closures whenever they hold enough removable mass.

pub mod compression;
#[cfg(test)]
mod test;

use crate::util::TokenEncoding;
use cosh_sdk::connector::{
    ChatMessage, ToolCallFunctionMsg, ToolCallMsg, assistant_tool_call_message,
    tool_result_message, user_message,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use text_splitter::TextSplitter;

use compression::init::{CHUNK_CAPACITY, init as deterministic_compress};

/// Default token budget for the total conversation context. Overridable via
/// [`ContextManager::new`].
pub const MAX_CONTEXT_TOKENS: usize = 100_000;

/// Percentage of the budget at which the four-phase compaction runs.
const COMPACT_PCT: usize = 80;

/// Percentage of the budget below which `trim_loop_closures` trims
/// `LoopClosure`s. The pipeline/draft/tool phases aim for the 80% trigger
/// instead — the minimum decompaction.
const TARGET_PCT: usize = 40;

/// TF-IDF maximum document frequency filter for deterministic compression.
const TFIDF_MAX_DF: f64 = 0.8;

/// MMR lambda balancing relevance and diversity for deterministic compression.
const MMR_LAMBDA: f64 = 0.7;

/// MMR compression ratio: keep roughly this fraction of the input chunks.
const MMR_RATIO: f64 = 0.4;

/// Maximum number of sentence-chunks per deterministic compression pass.
/// Larger inputs are split into batches to bound the O(n²) SVD/MMR cost.
const DETERMINISTIC_MAX_CHUNKS: usize = 200;

/// How many of the most recent user prompts are protected from compression
/// and from the compaction phases.
const PROTECTED_USER_PROMPTS: usize = 2;

/// The current task anchor: the FIRST user prompt of the session, or the
/// first user prompt after a [`ContextItem::LoopClosure`] (a new task
/// segment). It carries the user's original intent, so it is protected from
/// draft eviction while it is the anchor and is NEVER submitted to the
/// compression pipeline (it stays verbatim). It is not eternal: when a new
/// segment starts, the anchor moves to the new prompt and the old one becomes
/// an ordinary (removable) prompt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PromptAnchor {
    pub id: u64,
}

/// A single conversation item in display order. **One item = one message** in
/// [`ContextManager::build_messages`], so message positions are preserved by
/// construction and a compression swap is an in-place field update.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContextItem {
    /// A user prompt. The two most recent are `protected` (never compressed,
    /// never removed); older ones are compressed by the pipeline (phase 1 of
    /// the compaction) when the 80% budget overflows.
    User {
        id: u64,
        protected: bool,
        /// True when this prompt must NEVER be submitted to the compression
        /// pipeline (it stays in its original form): the task anchor prompts.
        verbatim: bool,
        original: String,
        /// Filled in place when the pipeline returns the compressed copy.
        compressed: Option<String>,
    },
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
    },
    /// A tool RESULT — structural, never prose-compressed. Renders as a
    /// `tool` message with the matching `tool_call_id`.
    ToolResult {
        id: u64,
        call_id: String,
        content: String,
        /// True when the tool produced no useful output (e.g. a zero-match
        /// search). Such a result served its purpose the moment the model
        /// reacted to it, so its chain is the first eviction target — even
        /// when it is the newest chain.
        ///
        /// Format note: bincode 1.x is positional (no field boundaries), so
        /// ANY added field is a format break — a snapshot from a build that
        /// predates this field fails the whole deserialization and the TUI
        /// falls back to the JSONL history (conversation recovered, only the
        /// compression/closure state is lost — the accepted dev-stage
        /// tradeoff).
        useless: bool,
    },
    /// The final text response of a completed agent loop. Replaces the raw
    /// assistant text in place; removed oldest-first by `trim_loop_closures`.
    /// Also marks the segment boundary for the pipeline and for the gradual
    /// draft eviction (phase 2 yields here).
    LoopClosure { id: u64, content: String },
}

impl ContextItem {
    fn id(&self) -> u64 {
        match self {
            ContextItem::User { id, .. }
            | ContextItem::Assistant { id, .. }
            | ContextItem::ToolCall { id, .. }
            | ContextItem::ToolResult { id, .. }
            | ContextItem::LoopClosure { id, .. } => *id,
        }
    }

    /// Estimated token cost of what this item currently renders, measured with
    /// the encoding for the active model (`enc` is resolved once by the
    /// manager via [`ContextManager::set_model`], not per item).
    fn tokens(&self, enc: TokenEncoding) -> usize {
        match self {
            ContextItem::User {
                original,
                compressed,
                ..
            }
            | ContextItem::Assistant {
                original,
                compressed,
                ..
            } => enc.estimate(compressed.as_deref().unwrap_or(original)),
            ContextItem::ToolCall {
                name, arguments, ..
            } => enc.estimate(name) + enc.estimate(arguments),
            ContextItem::ToolResult { content, .. } => enc.estimate(content),
            ContextItem::LoopClosure { content, .. } => enc.estimate(content),
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

    /// True when this item is a protected anchor (active user prompt) that must
    /// never be removed by the compaction phases.
    fn is_protected(&self) -> bool {
        matches!(
            self,
            ContextItem::User {
                protected: true,
                ..
            }
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
/// eviction phases remain the fallback.
fn try_compress(text: &str) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compress_text(text))).ok()
}

/// Snapshot of context manager state for bincode persistence. The compression
/// pipeline runs synchronously at the next 80% overflow, so nothing is
/// in-flight between save and restore — the snapshot is a plain clone.
#[derive(Serialize, Deserialize)]
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub user_prompts: VecDeque<u64>,
    pub max_tokens: usize,
    /// NOTE: bincode 1.x is positional and does NOT honor `#[serde(default)]`
    /// — a snapshot missing ANY field (mid-stream or trailing) fails the
    /// whole deserialization with an error, and the TUI falls back to the
    /// JSONL history (conversation recovered, compression/closure state
    /// lost). Adding a field is a format break; see the `useless` field.
    pub anchor: Option<PromptAnchor>,
}

/// Snapshot of context manager state for TUI display: the budget percentage
/// and a live token counter (rendered to the left of the budget bar).
#[derive(Default, Clone, Debug)]
pub struct ContextDisplayInfo {
    /// Total estimated tokens held by the context manager.
    pub total_tokens: usize,
    /// Percentage of the budget used (0-100).
    pub budget_pct: u8,
}

/// Which compaction phase LEADS the next budget overflow (the toggle).
///
/// The cycle is pipeline → draft eviction → tool eviction → back to the
/// pipeline. The draft pass is the sticky lead: it stays on itself across
/// consecutive overflows until it reaches a [`LoopClosure`] (its oldest
/// segment is exhausted), only then handing the lead to the tool pass. The
/// tool pass completing resets the cycle to the pipeline. Scheduling bias
/// only — not persisted; a restored manager starts the cycle at the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToggleLead {
    Pipeline,
    Drafts,
    Tools,
}

impl ToggleLead {
    /// The next phase in the toggle cycle (wraps `Tools` back to `Pipeline`
    /// — the funnel may "rotate to 1 again").
    fn next(self) -> Self {
        match self {
            ToggleLead::Pipeline => ToggleLead::Drafts,
            ToggleLead::Drafts => ToggleLead::Tools,
            ToggleLead::Tools => ToggleLead::Pipeline,
        }
    }
}

/// A compaction-phase notification emitted while [`ContextManager::run`]
/// executes, so the caller (the TUI) can show live feedback in the chat: a
/// stopwatch for the pipeline (phase 1 — the only phase slow enough to block
/// the agent loop) and one line per other phase that actually did work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionEvent {
    /// Phase 1 (the pipeline) is about to compress unprotected drafts — the
    /// TUI starts the stopwatch. Only emitted when there is real work.
    PipelineStarted,
    /// Phase 1 finished compressing — the TUI stops the stopwatch.
    PipelineFinished,
    /// Phase 2 (gradual draft eviction) removed at least one chunk.
    DraftsEvicted,
    /// Phase 3 (tool-chain eviction) removed at least one chain.
    ToolsEvicted,
    /// Phase 4 (loop-closure trimming) discarded at least one closure.
    ClosuresTrimmed,
}

/// Outcome of the gradual draft eviction pass (phase 2).
#[derive(Clone, Copy, Debug)]
enum DraftOutcome {
    /// The total dropped below the trigger; the cursor is parked on a regular
    /// item — phase 2's jurisdiction continues on the next overflow (the
    /// toggle stays on the draft pass).
    BelowTrigger,
    /// The pass reached a [`LoopClosure`] (or the end of the timeline): the
    /// oldest segment's drafts are exhausted. `below` tells the funnel whether
    /// the call is done (`true`) or must fall through to the next toggle
    /// phase (`false`). Either way the toggle hands the lead to the tool pass.
    AtBoundary { below: bool },
}

/// Orchestrates the synchronous TF-IDF → LSA → MMR compression of the whole
/// conversation, the in-place swaps, the 80/40 compaction phases, and the
/// `LoopClosure` promotion of each finished agent loop.
pub struct ContextManager {
    /// Conversation items in display order (oldest first). One item = one
    /// message; positions are stable.
    items: VecDeque<ContextItem>,
    /// Persistent position of the gradual draft eviction (phase 2): the id of
    /// the NEXT item to consider. `None` means the walk starts at the
    /// beginning of the timeline. The eviction removes one whole chunk per
    /// stop and parks this cursor, so the next budget overflow resumes exactly
    /// where this one stopped instead of jumping to the tool pass. Items the
    /// walk skipped while non-removable (protected prompts, the anchor) are
    /// behind the cursor, but whenever the timeline is exhausted while still
    /// over the trigger the cursor resets to `None` and the next session walks
    /// from the start — so a prompt that only became removable later is still
    /// evicted eventually. Scheduling bias only — not persisted; a restored
    /// manager simply starts over.
    draft_cursor: Option<u64>,
    /// Which compaction phase leads the next budget overflow (the toggle):
    /// pipeline → draft eviction → tool eviction → pipeline. Phase 2 is the
    /// sticky lead — it stays across consecutive overflows (one chunk per
    /// stop) until it reaches a [`LoopClosure`]; the tool pass completing
    /// resets the cycle. Scheduling bias only — not persisted; a restored
    /// manager starts the cycle at the pipeline.
    toggle_lead: ToggleLead,
    /// Live compaction notifications for the TUI: called synchronously while
    /// [`Self::run`] executes so the caller can show the pipeline stopwatch
    /// and one line per phase that actually did work. Scheduling-only — not
    /// part of the snapshot; the harness wires it per agent loop via
    /// [`Self::set_compaction_observer`]. `Send` so a [`Harness`] holding the
    /// manager stays movable into a tokio task.
    compaction_observer: Option<Box<dyn FnMut(CompactionEvent) + Send>>,
    /// The current task anchor (the first prompt of the session or the first
    /// prompt after a LoopClosure). Protected from draft eviction while
    /// current; the anchored prompt is marked verbatim (never compressed).
    /// Moves to the next segment's first prompt when a new segment starts.
    /// Persisted across save/restore.
    anchor: Option<PromptAnchor>,
    /// Monotonic id counter for items/jobs.
    next_id: u64,
    /// Ids of the most recent user prompts (oldest first), capped at
    /// `PROTECTED_USER_PROMPTS`. These are never compressed nor removed.
    user_prompts: VecDeque<u64>,
    /// Token budget before the 80% compaction trigger.
    max_tokens: usize,
    /// tiktoken encoding matching the active model (set by the harness via
    /// [`Self::set_model`]). Defaults to [`TokenEncoding::Cl100k`] — the
    /// generic cross-provider estimate.
    encoding: TokenEncoding,
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
            toggle_lead: ToggleLead::Pipeline,
            compaction_observer: None,
            anchor: None,
            next_id: 1,
            user_prompts: VecDeque::new(),
            max_tokens,
            encoding: TokenEncoding::Cl100k,
        }
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

    // Ingestion

    /// Add a user prompt. The two most recent prompts are protected (raw,
    /// never compressed); when a third one arrives, the oldest protected
    /// prompt loses protection and becomes a plain draft — eligible for
    /// synchronous compression by the pipeline (phase 1) at the next 80%
    /// overflow, or for eviction by the draft pass.
    pub fn add_user(&mut self, text: &str) {
        if self.user_prompts.len() == PROTECTED_USER_PROMPTS {
            let oldest = self.user_prompts.pop_front().unwrap();
            self.unprotect(oldest);
        }
        let id = self.next_id();
        // A new task segment starts at the session's first prompt and after
        // each closed loop (a LoopClosure): that prompt becomes the task
        // anchor — marked verbatim (never compressed) and protected from
        // draft eviction. When the anchor moves to a new segment's prompt,
        // the old one becomes an ordinary removable prompt again.
        let is_segment_start = self.items.is_empty()
            || matches!(self.items.back(), Some(ContextItem::LoopClosure { .. }));
        if is_segment_start {
            self.anchor = Some(PromptAnchor { id });
        }
        self.user_prompts.push_back(id);
        self.items.push_back(ContextItem::User {
            id,
            protected: true,
            verbatim: is_segment_start,
            original: text.to_string(),
            compressed: None,
        });
    }

    /// Add an assistant text output. When `compressible` is true the raw text
    /// is rendered in place until the pipeline (phase 1 of the compaction)
    /// summarizes it — synchronously, at the next 80% overflow. When false
    /// (the output carried a tool call) it stays structural and is never
    /// submitted to the compressor.
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
        let id = self.next_id();
        self.items.push_back(ContextItem::ToolCall {
            id,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        });
    }

    /// Add a tool RESULT. Structural, never prose-compressed.
    pub fn add_tool_result(&mut self, call_id: &str, content: &str) {
        self.add_tool_result_flagged(call_id, content, false);
    }

    /// Add a tool RESULT with an explicit `useless` marker. Useless chains are
    /// the first eviction targets of the tool pass (see [`Self::evict_tools`]);
    /// everything else behaves exactly like [`Self::add_tool_result`].
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

    // Compaction phases

    /// Per-iteration tick: run the 80% compaction when the held context
    /// reaches the trigger.
    ///
    /// A **toggle** rotates which phase LEADS each overflow, and the funnel
    /// falls through the toggle cycle — wrapping to the pipeline if needed —
    /// until the total drops below the 80% trigger:
    ///
    ///   1. **The pipeline** — unprotected drafts are summarized synchronously
    ///      on the agent loop's thread, segment by segment (the budget is
    ///      checked at each [`LoopClosure`] boundary);
    ///   2. **Draft eviction** — one whole chunk (a full user input or
    ///      assistant output) at a time from a persistent cursor, stopping the
    ///      moment the total drops below 80%. The cursor makes the NEXT
    ///      overflow continue where this one stopped; only a [`LoopClosure`]
    ///      boundary (the oldest segment's drafts are exhausted) ends phase
    ///      2's jurisdiction and hands the lead to the tool pass;
    ///   3. **Tool eviction** — tool chains, middle-out. Completing it resets
    ///      the toggle to the pipeline;
    ///   4. **LoopClosure trimming** — outside the toggle, always evaluated
    ///      last (fires only when closures alone hold ≥ 40% of the budget).
    ///
    /// After the call the toggle advances for the next overflow: the tool
    /// pass completing resets to the pipeline; otherwise a phase-2 boundary
    /// hands the lead to the tool pass; otherwise the draft pass stays the
    /// lead (phase 1 resolving, or phase 2 still mid-segment, both hand or
    /// keep the lead on phase 2).
    ///
    /// Phases 1-3 aim for the 80% trigger (minimum decompaction): a pass ends
    /// just under the ceiling, so in a busy tool loop the next `run()` — called
    /// after every dispatch — re-triggers with a minimal removal each time.
    /// That is the intended rolling eviction, not a bug.
    pub fn run(&mut self) {
        if self.total_tokens() < self.trigger() {
            return;
        }
        // The funnel: lead with the toggle's current phase, and if it cannot
        // resolve (total still ≥ trigger), fall through to the NEXT phase of
        // the toggle cycle, wrapping to the pipeline if needed. Each phase
        // runs at most once per call.
        let mut lead = self.toggle_lead;
        let mut tools_resolved = false; // phase 3 completed the job
        let mut drafts_boundary = false; // phase 2 reached a LoopClosure/end
        let mut attempts = 0;
        loop {
            let resolved = match lead {
                ToggleLead::Pipeline => self.pipeline_pass(),
                ToggleLead::Drafts => match self.evict_drafts() {
                    DraftOutcome::BelowTrigger => true,
                    DraftOutcome::AtBoundary { below } => {
                        drafts_boundary = true;
                        below
                    }
                },
                ToggleLead::Tools => {
                    // Each phase runs at most once per call, so a plain
                    // assignment is enough.
                    let ok = self.evict_tools();
                    tools_resolved = ok;
                    ok
                }
            };
            if resolved {
                break;
            }
            lead = lead.next();
            attempts += 1;
            if attempts >= 3 {
                break;
            }
        }
        // Advance the toggle for the NEXT overflow.
        self.toggle_lead = if tools_resolved {
            // Phase 3 completed (as the lead or via the funnel) → the cycle
            // restarts at the pipeline.
            ToggleLead::Pipeline
        } else if drafts_boundary {
            // Phase 2 exhausted its oldest segment → the tool pass leads next.
            ToggleLead::Tools
        } else {
            // Phase 1 resolved (hand the lead to phase 2) or phase 2 resolved
            // mid-segment (keep the lead) → the draft pass leads next.
            ToggleLead::Drafts
        };
        // Phase 4 — LoopClosure trimming, outside the toggle, always last.
        self.trim_loop_closures();
    }

    /// Phase 1 of the compaction: the compression pipeline, now synchronous.
    ///
    /// Walks the timeline oldest-first and compresses every unprotected
    /// draft — un-protected, non-verbatim user prompts and compressible
    /// assistant outputs that are still raw — **segment by segment**. The
    /// budget is checked at each [`ContextItem::LoopClosure`] boundary: below
    /// the 80% trigger → the pass is done (drafts beyond the boundary stay
    /// raw — the minimum decompaction); still over → the walk continues into
    /// the next segment. A segment's drafts are compressed as a coherent
    /// block, so the model sees a consistent (compressed) view of each
    /// completed loop instead of a mixed raw/summary mix.
    ///
    /// Returns `true` when the total dropped below the trigger (the funnel
    /// stops here); `false` when the timeline was exhausted still over the
    /// trigger — the funnel falls through to the next toggle phase
    /// (`evict_drafts`, then the tool pass).
    ///
    /// Runs on the agent loop's thread — there is no worker thread anymore.
    fn pipeline_pass(&mut self) -> bool {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            return true;
        }
        // Only report the pass when there is at least one compressible draft —
        // an empty pass is instant and invisible to the user, so it must not
        // surface a stopwatch line.
        let has_work = self.items.iter().any(|it| {
            matches!(
                it,
                ContextItem::User {
                    protected: false,
                    verbatim: false,
                    compressed: None,
                    ..
                } | ContextItem::Assistant {
                    compressible: true,
                    compressed: None,
                    ..
                }
            )
        });
        if has_work {
            self.emit(CompactionEvent::PipelineStarted);
        }
        let mut idx = 0;
        while idx < self.items.len() {
            if self.items[idx].is_loop() {
                // Segment boundary: a whole loop's drafts have been
                // summarized. Below the trigger → done; still over → the next
                // segment's drafts are the next candidates.
                if total < trigger {
                    if has_work {
                        self.emit(CompactionEvent::PipelineFinished);
                    }
                    return true;
                }
                idx += 1;
                continue;
            }
            let eligible = matches!(
                &self.items[idx],
                ContextItem::User {
                    protected: false,
                    verbatim: false,
                    compressed: None,
                    ..
                } | ContextItem::Assistant {
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
                ContextItem::User { original, .. } | ContextItem::Assistant { original, .. } => {
                    original.clone()
                }
                _ => unreachable!(),
            };
            let Some(compressed) = try_compress(&original) else {
                // Degrade: keep the draft raw — the eviction phases still
                // apply.
                idx += 1;
                continue;
            };
            let old_tokens = self.items[idx].tokens(self.encoding);
            match &mut self.items[idx] {
                ContextItem::User { compressed: c, .. }
                | ContextItem::Assistant { compressed: c, .. } => *c = Some(compressed),
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

    /// Evict tool chains from the middle outward (Goose technique) until the
    /// total drops below the 80% trigger.
    ///
    /// Useless chains (results marked `useless`, e.g. a zero-match search) are
    /// evicted FIRST — a zero-value result carried its one bit of information
    /// the moment the model read it, so keeping its chain burns context on
    /// every subsequent call, even when it is the newest chain. The middle-out
    /// order is preserved within each partition, so non-useless chains keep
    /// their existing eviction behavior exactly.
    ///
    /// Returns `true` when the total dropped below the trigger; `false` when
    /// there is nothing left to remove (or nothing to remove at all) — the
    /// funnel then falls through to the next toggle phase (wrapping to the
    /// pipeline).
    fn evict_tools(&mut self) -> bool {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            return true;
        }
        let tool_ids: Vec<u64> = self
            .items
            .iter()
            .filter(|it| it.is_tool())
            .map(ContextItem::id)
            .collect();
        if tool_ids.is_empty() {
            return false;
        }
        // Precompute the useless flag per id ONCE — the sort below would
        // otherwise re-scan the whole item list for every id (O(n²) on the
        // compaction path of long tool-heavy sessions).
        let useless_flags: Vec<bool> = tool_ids
            .iter()
            .map(|id| self.chain_is_useless(*id))
            .collect();
        // Expand outward from the middle with two pointers so EVERY index is
        // covered. (A previous alternating scheme dropped the newest tool item
        // whenever the count was odd — n=1 produced an empty order.) The order
        // holds INDICES into `tool_ids` so the useless flags stay aligned.
        let middle = tool_ids.len() / 2;
        let mut order: Vec<usize> = Vec::with_capacity(tool_ids.len());
        let mut left = middle;
        let mut right = middle;
        while left > 0 || right < tool_ids.len() {
            if left > 0 {
                left -= 1;
                order.push(left);
            }
            if right < tool_ids.len() {
                order.push(right);
                right += 1;
            }
        }
        // Stable partition: useless chains first, middle-out order kept within
        // each partition (a no-op when nothing is marked useless).
        order.sort_by_key(|idx| !useless_flags[*idx]);
        let mut removed_any = false;
        for idx in order {
            let id = tool_ids[idx];
            if total < trigger {
                break;
            }
            if let Some(pos) = self.items.iter().position(|it| it.id() == id) {
                total = total.saturating_sub(self.remove_item(pos));
                removed_any = true;
            }
        }
        if removed_any {
            self.emit(CompactionEvent::ToolsEvicted);
        }
        total < trigger
    }

    /// Phase 2 of the compaction: gradual draft eviction.
    ///
    /// Removes ONE whole chunk — a full user input or a full assistant output
    /// — at a time, walking the timeline oldest-first from a persistent
    /// cursor. After each removal the budget is checked: below the 80% trigger
    /// → stop (the cursor parks on the next item, so the NEXT overflow resumes
    /// exactly here — the decoupling is spread over many overflows and the
    /// context lives much longer).
    ///
    /// Returns [`DraftOutcome::BelowTrigger`] when it stopped below the
    /// trigger with the cursor parked on a regular item — the toggle stays on
    /// the draft pass. Returns [`DraftOutcome::AtBoundary`] when it reaches a
    /// [`ContextItem::LoopClosure`] (the oldest segment's drafts are
    /// exhausted — the jurisdiction ends whether or not the total also dropped
    /// below the trigger, `below`) or when the end of the timeline is reached
    /// while STILL over the trigger (nothing left to evict). Ending the
    /// timeline below the trigger is a plain `BelowTrigger`: the cursor resets
    /// to `None` so the next overflow re-walks from the start, revisiting
    /// items that only became removable later.
    ///
    /// Cursor parking: the walk-yield at a closure parks PAST it (`idx + 1`,
    /// the next segment starts there), while a below-the-trigger stop with a
    /// closure as the next item parks ON the closure — both are safe because
    /// the resume looks for the first item with `id >= cursor`.
    ///
    /// Never removes [`LoopClosure`]s, protected prompts, the current
    /// [`PromptAnchor`] or tool chains.
    fn evict_drafts(&mut self) -> DraftOutcome {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            // Defensive — `run` only calls this over the trigger.
            return DraftOutcome::BelowTrigger;
        }
        // Resume from the persistent cursor (the id of the next item to
        // consider), falling back to the start of the timeline. Ids are
        // monotonic, so the first item with `id >= cursor` is the resume point
        // even if the cursor item itself was removed by another phase.
        let mut idx = self
            .draft_cursor
            .map(|cid| {
                self.items
                    .iter()
                    .position(|it| it.id() >= cid)
                    .unwrap_or(self.items.len())
            })
            .unwrap_or(0);
        let mut removed_any = false;
        loop {
            // Advance past the non-removable items. A LoopClosure is the
            // segment boundary: phase 2 yields here so the toggle can move on.
            while idx < self.items.len() {
                if self.items[idx].is_loop() {
                    self.draft_cursor = self.items.get(idx + 1).map(|it| it.id());
                    if removed_any {
                        self.emit(CompactionEvent::DraftsEvicted);
                    }
                    return DraftOutcome::AtBoundary {
                        below: total < trigger,
                    };
                }
                if !self.is_removable_draft(idx) {
                    idx += 1;
                    continue;
                }
                break;
            }
            if idx >= self.items.len() {
                // End of the timeline — no checkpoint ahead. Below the trigger
                // → resolved, and the cursor resets so the next overflow
                // re-walks from the start (revisiting items that only became
                // removable later — the documented convergence). Still over
                // → nothing left to evict: a boundary that hands the lead to
                // the tool pass.
                self.draft_cursor = None;
                if removed_any {
                    self.emit(CompactionEvent::DraftsEvicted);
                }
                return if total < trigger {
                    DraftOutcome::BelowTrigger
                } else {
                    DraftOutcome::AtBoundary { below: false }
                };
            }
            // Remove ONE chunk and check the budget immediately.
            let removed = self.remove_item(idx);
            total = total.saturating_sub(removed);
            removed_any = true;
            // The next item shifted into `idx`; park the cursor there.
            self.draft_cursor = self.items.get(idx).map(|it| it.id());
            if total < trigger {
                // Below the trigger. If the very next item is a LoopClosure,
                // the segment's drafts are exhausted too — phase 2's
                // jurisdiction ends even though the budget was resolved (the
                // walkthrough's "hit the checkpoint and the phase is done").
                if removed_any {
                    self.emit(CompactionEvent::DraftsEvicted);
                }
                return match self.items.get(idx) {
                    Some(it) if it.is_loop() => DraftOutcome::AtBoundary { below: true },
                    _ => DraftOutcome::BelowTrigger,
                };
            }
        }
    }

    /// True when the item at `idx` is a removable draft chunk: a user input or
    /// an assistant output that is not a protected prompt, the current task
    /// anchor or a tool-chain item. `LoopClosure`s are never removed here —
    /// they are the segment boundary that ends phase 2's jurisdiction.
    fn is_removable_draft(&self, idx: usize) -> bool {
        let item = &self.items[idx];
        if item.is_loop() || item.is_protected() || item.is_tool() {
            return false;
        }
        !self.anchor.is_some_and(|a| a.id == item.id())
    }

    /// Trim `LoopClosure`s: when they alone hold ≥ 40% of the budget, discard
    /// the oldest one at a time until the SUM of `LoopClosure`s drops below
    /// 40%. The 40% limit applies only to the closures (the final summaries),
    /// never to the whole context.
    fn trim_loop_closures(&mut self) {
        let target = self.target();
        let mut loop_tokens: usize = self
            .items
            .iter()
            .filter(|it| it.is_loop())
            .map(|it| it.tokens(self.encoding))
            .sum();
        if loop_tokens < target {
            return;
        }
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter(|it| it.is_loop())
            .map(ContextItem::id)
            .collect();
        let mut removed_any = false;
        for id in ids {
            if loop_tokens < target {
                break;
            }
            if let Some(idx) = self.items.iter().position(|it| it.id() == id) {
                let removed = self.remove_item(idx);
                loop_tokens = loop_tokens.saturating_sub(removed);
                removed_any = true;
            }
        }
        if removed_any {
            self.emit(CompactionEvent::ClosuresTrimmed);
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
                ContextItem::User {
                    original,
                    compressed,
                    ..
                } => {
                    // User prompts keep the native `user` role; the compressed
                    // copy (if ready) replaces the text in place.
                    messages.push(user_message(compressed.as_deref().unwrap_or(original)));
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
                    ..
                } => {
                    messages.push(assistant_tool_call_message(vec![ToolCallMsg {
                        id: call_id.clone(),
                        kind: "function".to_string(),
                        function: ToolCallFunctionMsg {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
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
        }
    }

    /// Snapshot of the current items (test accessor for the harness test
    /// module, which is not a submodule of the context manager).
    #[cfg(test)]
    pub(crate) fn items_snapshot(&self) -> Vec<ContextItem> {
        self.items.iter().cloned().collect()
    }

    /// The toggle's current lead phase (test accessor). Private — only the
    /// child `test` module needs it, and it returns the module-private
    /// [`ToggleLead`], so a broader visibility would trip `private_interfaces`.
    #[cfg(test)]
    fn toggle_for_test(&self) -> ToggleLead {
        self.toggle_lead
    }

    /// Serializable snapshot for bincode persistence. The compression
    /// pipeline runs synchronously at the next overflow, so there is no
    /// in-flight work to exclude.
    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            items: self.items.clone(),
            next_id: self.next_id,
            user_prompts: self.user_prompts.clone(),
            max_tokens: self.max_tokens,
            anchor: self.anchor,
        }
    }

    /// Restore a previously saved snapshot. The compression pipeline runs
    /// synchronously at the next 80% overflow, so no re-submission is needed —
    /// the items are restored verbatim and the pipeline summarizes them (or
    /// the eviction phases remove them) exactly as in a fresh session.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.items = state.items.clone();
        self.next_id = state.next_id;
        self.user_prompts = state.user_prompts.clone();
        self.max_tokens = state.max_tokens;
        self.anchor = state.anchor;
        // Scheduling bias only — a restored session restarts the gradual
        // draft eviction from the beginning of the timeline and the toggle
        // cycle at the pipeline.
        self.draft_cursor = None;
        self.toggle_lead = ToggleLead::Pipeline;
        // Defensive: whatever the snapshot claims as the anchor must render
        // verbatim, even if a hand-built state left the flag unset.
        if let Some(anchor) = self.anchor
            && let Some(idx) = self.items.iter().position(|it| it.id() == anchor.id)
            && let ContextItem::User { verbatim, .. } = &mut self.items[idx]
        {
            *verbatim = true;
        }
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

    /// Token count the compaction aims for after phases 1-2 (40% of the
    /// budget).
    fn target(&self) -> usize {
        self.max_tokens.saturating_mul(TARGET_PCT) / 100
    }

    fn total_tokens(&self) -> usize {
        self.items.iter().map(|it| it.tokens(self.encoding)).sum()
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

    /// Un-protect a previously protected user prompt (now no longer among the
    /// two most recent): it becomes an ordinary draft. Compression is no
    /// longer triggered here — the pipeline (phase 1 of the compaction)
    /// summarizes unprotected drafts synchronously when the 80% budget
    /// overflows, so this is purely a bookkeeping flip. Verbatim prompts (the
    /// task anchor, or a former anchor) stay in their original form wherever
    /// they sit — never compressed.
    fn unprotect(&mut self, id: u64) {
        let Some(idx) = self.items.iter().position(|it| it.id() == id) else {
            return;
        };
        let ContextItem::User {
            id,
            protected: true,
            verbatim,
            original,
            ..
        } = &self.items[idx]
        else {
            return;
        };
        self.items[idx] = ContextItem::User {
            id: *id,
            protected: false,
            verbatim: *verbatim,
            original: original.clone(),
            compressed: None,
        };
    }
}
