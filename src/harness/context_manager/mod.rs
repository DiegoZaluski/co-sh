//! Single-owner LLM-free asynchronous context manager.
//!
//! The [`ContextManager`] is the **single owner** of the whole conversation —
//! user prompts, assistant outputs, tool calls and tool results — before it
//! reaches the LLM. Fresh content is handed to a background worker thread that
//! compresses it with the deterministic TF-IDF → LSA → MMR pipeline
//! ([`compression::init`]); the original text stays rendered in place until the
//! compressed copy returns and is swapped in at the **same position** — a field
//! update, so the message order is never reorganized.
//!
//! [`build_messages`](Self::build_messages) renders the conversation as a
//! provider-ready `Vec<ChatMessage>` with a **1:1 mapping** between items and
//! messages, so every message keeps its original position by construction.
//!
//! When the held context reaches 80% of the single budget, three phases run:
//!
//!   1. **Phase 1** — tool items are removed from the middle outward (the same
//!      idea Goose uses) until the total drops below the trigger;
//!   2. **Phase 2** — draft items (anything that is not a [`LoopClosure`] or a
//!      protected user prompt) are removed oldest-first until the total is
//!      ≤ 40% of the budget;
//!   3. **Phase 3** — if `LoopClosure`s alone hold ≥ 40% of the budget, the
//!      oldest `LoopClosure` is discarded until the total drops below 40%.
//!
//! The two most recent user prompts are *protected* (never compressed, never
//! removed). The final text response of each completed agent loop is promoted
//! by [`close_loop`](Self::close_loop) into a `LoopClosure` that replaces the
//! raw text in place.

pub mod compression;
#[cfg(test)]
mod test;

use crate::util::token_counter::estimate_tokens;
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

/// Percentage of the budget at which the three-phase compaction runs.
const COMPACT_PCT: usize = 80;

/// Percentage of the budget the compaction aims for after phases 1-2.
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

/// A single conversation item in display order. **One item = one message** in
/// [`ContextManager::build_messages`], so message positions are preserved by
/// construction and a compression swap is an in-place field update.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContextItem {
    /// A user prompt. The two most recent are `protected` (never compressed,
    /// never removed); older ones are compressed asynchronously.
    User {
        id: u64,
        protected: bool,
        original: String,
        /// Filled in place when the worker returns the compressed copy.
        compressed: Option<String>,
    },
    /// An assistant text output.
    Assistant {
        id: u64,
        original: String,
        /// Filled in place when the worker returns the compressed copy.
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
    },
    /// The final text response of a completed agent loop. Replaces the raw
    /// assistant text in place; removed oldest-first by Phase 3.
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

    /// Estimated token cost of what this item currently renders.
    fn tokens(&self) -> usize {
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
            } => estimate_tokens(compressed.as_deref().unwrap_or(original)),
            ContextItem::ToolCall {
                name, arguments, ..
            } => estimate_tokens(name) + estimate_tokens(arguments),
            ContextItem::ToolResult { content, .. } => estimate_tokens(content),
            ContextItem::LoopClosure { content, .. } => estimate_tokens(content),
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

// Compression worker thread

/// A job handed to the worker thread.
struct CompressJob {
    id: u64,
    text: String,
}

/// A completed compression, matched back to its item by `id`.
struct CompressResult {
    id: u64,
    compressed: String,
}

/// Deterministic compression with batching for very large inputs, so a huge
/// item can never stall the agent loop with an unbounded SVD/MMR pass.
fn compress_text(text: &str) -> String {
    if estimate_tokens(text) <= MAX_CONTEXT_TOKENS {
        return deterministic_compress(text, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO);
    }

    let splitter = TextSplitter::new(CHUNK_CAPACITY);
    let chunks: Vec<&str> = splitter.chunks(text).collect();
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

/// Spawn the compressor thread and return its work/result channels.
/// The thread exits when the work sender is dropped.
fn spawn_compressor() -> (
    std::sync::mpsc::Sender<CompressJob>,
    std::sync::mpsc::Receiver<CompressResult>,
) {
    let (work_tx, work_rx) = std::sync::mpsc::channel::<CompressJob>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<CompressResult>();
    std::thread::spawn(move || {
        for job in work_rx {
            // A panic in the deterministic pipeline (e.g. a degenerate SVD or
            // text-splitter edge case) must not kill the worker thread — if it
            // did, every subsequent job would fail silently and the item would
            // stay raw forever. Degrade to the original text instead and keep
            // processing.
            let compressed =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compress_text(&job.text)))
                    .unwrap_or_else(|_| job.text.clone());
            if result_tx
                .send(CompressResult {
                    id: job.id,
                    compressed,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (work_tx, result_rx)
}

/// Snapshot of context manager state for bincode persistence. Channels and the
/// worker thread are rebuilt on restore; in-flight items are re-submitted.
#[derive(Serialize, Deserialize)]
pub struct ContextManagerState {
    pub items: VecDeque<ContextItem>,
    pub next_id: u64,
    pub user_prompts: VecDeque<u64>,
    pub max_tokens: usize,
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

/// Orchestrates the asynchronous TF-IDF → LSA → MMR compression of the whole
/// conversation, the in-place swaps, the 80/40 compaction phases, and the
/// `LoopClosure` promotion of each finished agent loop.
pub struct ContextManager {
    /// Conversation items in display order (oldest first). One item = one
    /// message; positions are stable.
    items: VecDeque<ContextItem>,
    /// Work queue to the compressor thread.
    work_tx: std::sync::mpsc::Sender<CompressJob>,
    /// Completed compressions, drained on each poll.
    result_rx: std::sync::mpsc::Receiver<CompressResult>,
    /// Monotonic id counter for items/jobs.
    next_id: u64,
    /// Ids of the most recent user prompts (oldest first), capped at
    /// `PROTECTED_USER_PROMPTS`. These are never compressed nor removed.
    user_prompts: VecDeque<u64>,
    /// Token budget before the 80% compaction trigger.
    max_tokens: usize,
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
        let (work_tx, result_rx) = spawn_compressor();
        Self {
            items: VecDeque::new(),
            work_tx,
            result_rx,
            next_id: 1,
            user_prompts: VecDeque::new(),
            max_tokens,
        }
    }

    // Ingestion

    /// Add a user prompt. The two most recent prompts are protected (raw,
    /// never compressed); when a third one arrives, the oldest protected prompt
    /// loses protection and is submitted for compression.
    pub fn add_user(&mut self, text: &str) {
        self.poll();
        if self.user_prompts.len() == PROTECTED_USER_PROMPTS {
            let oldest = self.user_prompts.pop_front().unwrap();
            self.submit_for_compression(oldest);
        }
        let id = self.next_id();
        self.user_prompts.push_back(id);
        self.items.push_back(ContextItem::User {
            id,
            protected: true,
            original: text.to_string(),
            compressed: None,
        });
    }

    /// Add an assistant text output. When `compressible` is true the raw text
    /// is rendered in place until the compressed copy returns (async); when
    /// false (the output carried a tool call) it stays structural and is never
    /// submitted to the compressor. If the worker is unavailable the item
    /// simply stays uncompressed — nothing is lost.
    pub fn add_assistant(&mut self, text: &str, compressible: bool) {
        self.poll();
        let id = self.next_id();
        if compressible {
            let job = CompressJob {
                id,
                text: text.to_string(),
            };
            let _ = self.work_tx.send(job);
        }
        self.items.push_back(ContextItem::Assistant {
            id,
            original: text.to_string(),
            compressed: None,
            compressible,
        });
    }

    /// Add a tool CALL. Structural, never prose-compressed.
    pub fn add_tool_call(&mut self, call_id: &str, name: &str, arguments: &str) {
        self.poll();
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
        self.poll();
        let id = self.next_id();
        self.items.push_back(ContextItem::ToolResult {
            id,
            call_id: call_id.to_string(),
            content: content.to_string(),
        });
    }

    /// Poll the result queue, swapping each returned compressed copy into the
    /// place of its original (a field update — the list is never shifted).
    /// Non-blocking; finding the item is a linear scan over the items.
    pub fn poll(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            let Some(idx) = self.items.iter().position(|it| it.id() == result.id) else {
                continue;
            };
            match &mut self.items[idx] {
                ContextItem::User { compressed, .. }
                | ContextItem::Assistant {
                    compressed,
                    compressible: true,
                    ..
                } => {
                    *compressed = Some(result.compressed);
                }
                _ => {
                    // Structural items are never compressed — drop stray results.
                }
            }
        }
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

    /// Per-iteration tick: drain finished compressions, then run the 80%
    /// three-phase compaction when the held context reaches the trigger.
    pub fn run(&mut self) {
        self.poll();
        if self.total_tokens() >= self.trigger() {
            self.phase1();
            self.phase2();
            self.phase3();
        }
    }

    /// Phase 1: remove tool items from the middle outward (Goose technique)
    /// until the total drops below the 80% trigger.
    fn phase1(&mut self) {
        let trigger = self.trigger();
        let mut total = self.total_tokens();
        if total < trigger {
            return;
        }
        let tool_ids: Vec<u64> = self
            .items
            .iter()
            .filter(|it| it.is_tool())
            .map(ContextItem::id)
            .collect();
        if tool_ids.is_empty() {
            return;
        }
        // Expand outward from the middle with two pointers so EVERY index is
        // covered. (A previous alternating scheme dropped the newest tool item
        // whenever the count was odd — n=1 produced an empty order.)
        let middle = tool_ids.len() / 2;
        let mut order = Vec::with_capacity(tool_ids.len());
        let mut left = middle;
        let mut right = middle;
        while left > 0 || right < tool_ids.len() {
            if left > 0 {
                left -= 1;
                order.push(tool_ids[left]);
            }
            if right < tool_ids.len() {
                order.push(tool_ids[right]);
                right += 1;
            }
        }
        for id in order {
            if total < trigger {
                break;
            }
            if let Some(idx) = self.items.iter().position(|it| it.id() == id) {
                total = total.saturating_sub(self.remove_item(idx));
            }
        }
    }

    /// Phase 2: remove drafts (non-`LoopClosure`, non-protected items)
    /// oldest-first until the total is ≤ 40% of the budget.
    fn phase2(&mut self) {
        let target = self.target();
        let mut total = self.total_tokens();
        if total <= target {
            return;
        }
        let ids: Vec<u64> = self.items.iter().map(ContextItem::id).collect();
        for id in ids {
            if total <= target {
                break;
            }
            let Some(idx) = self.items.iter().position(|it| it.id() == id) else {
                continue;
            };
            if self.items[idx].is_loop() || self.items[idx].is_protected() {
                // LoopClosures are the final summaries of completed loops and
                // protected prompts are the active anchors — never removed.
                continue;
            }
            total = total.saturating_sub(self.remove_item(idx));
        }
    }

    /// Phase 3: when `LoopClosure`s alone hold ≥ 40% of the budget, discard the
    /// oldest `LoopClosure` until the total drops below 40%.
    fn phase3(&mut self) {
        let target = self.target();
        let loop_tokens: usize = self
            .items
            .iter()
            .filter(|it| it.is_loop())
            .map(ContextItem::tokens)
            .sum();
        if loop_tokens < target {
            return;
        }
        let mut total = self.total_tokens();
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter(|it| it.is_loop())
            .map(ContextItem::id)
            .collect();
        for id in ids {
            if total < target {
                break;
            }
            if let Some(idx) = self.items.iter().position(|it| it.id() == id) {
                total = total.saturating_sub(self.remove_item(idx));
            }
        }
    }

    // Rendering & persistence

    /// Render the whole conversation as provider-ready messages. One pass over
    /// the items (O(n) once per request); each item maps 1:1 to a message, so
    /// positions are preserved. Polls finished compressions first so the view
    /// is fresh.
    ///
    /// `current_input` is the harness's per-iteration steering message (e.g.
    /// "please continue with the tool results"): it is appended as a final
    /// `user` message, unless it duplicates the trailing user turn (which the
    /// harness already owns via [`add_user`](Self::add_user)).
    pub fn build_messages(&mut self, current_input: &str) -> Vec<ChatMessage> {
        self.poll();
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

    /// Serializable snapshot for bincode persistence.
    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            items: self.items.clone(),
            next_id: self.next_id,
            user_prompts: self.user_prompts.clone(),
            max_tokens: self.max_tokens,
        }
    }

    /// Restore a previously saved snapshot, rebuilding the worker thread and
    /// re-submitting every item that was still awaiting compression.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.items = state.items.clone();
        self.next_id = state.next_id;
        self.user_prompts = state.user_prompts.clone();
        self.max_tokens = state.max_tokens;
        let (work_tx, result_rx) = spawn_compressor();
        self.work_tx = work_tx;
        self.result_rx = result_rx;
        for item in &self.items {
            let job = match item {
                // Only UNPROTECTED user prompts are re-submitted — protected
                // prompts (the active session anchors) must never be compressed,
                // so they stay verbatim across a save/restore.
                ContextItem::User {
                    id,
                    original,
                    protected: false,
                    compressed: None,
                    ..
                }
                | ContextItem::Assistant {
                    id,
                    original,
                    compressed: None,
                    compressible: true,
                    ..
                } => Some((*id, original.clone())),
                _ => None,
            };
            if let Some((id, text)) = job {
                let _ = self.work_tx.send(CompressJob { id, text });
            }
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
        self.items.iter().map(ContextItem::tokens).sum()
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
            let tokens = self.items[idx].tokens();
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
                let total = self.items[lo].tokens() + self.items[hi].tokens();
                self.items.remove(hi);
                self.items.remove(lo);
                total
            }
            // No partner (orphan) — remove just this item.
            None => {
                let tokens = self.items[idx].tokens();
                self.items.remove(idx);
                tokens
            }
        }
    }

    /// Promote a previously protected user prompt (now no longer among the two
    /// most recent) into the compressor queue.
    fn submit_for_compression(&mut self, id: u64) {
        let Some(idx) = self.items.iter().position(|it| it.id() == id) else {
            return;
        };
        let ContextItem::User {
            id,
            protected: true,
            original,
            ..
        } = &self.items[idx]
        else {
            return;
        };
        let id = *id;
        let original = original.clone();
        if self
            .work_tx
            .send(CompressJob {
                id,
                text: original.clone(),
            })
            .is_err()
        {
            // Worker unavailable — keep the prompt protected so it is never lost.
            return;
        }
        self.items[idx] = ContextItem::User {
            id,
            protected: false,
            original,
            compressed: None,
        };
    }
}
