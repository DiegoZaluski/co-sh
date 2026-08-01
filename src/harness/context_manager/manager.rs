//! Layered context management system that progressively compresses conversation
//! history into a circular queue instead of discarding it. Models can seamlessly
//! navigate between compressed and expanded context through tools.
//!
//! Nothing is lost—only compressed, and always navigable.
//!
use crate::util::token_counter::estimate_tokens;
use cosh_sdk::connector::Connector;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use text_splitter::TextSplitter;

use super::compression::init as deterministic_compress;

/// Maximum token budget for the total conversation context.
const MAX_CONTEXT_TOKENS: usize = 10_000;

/// Maximum token budget for a seed entry. Once an entry compresses to
/// this size it leaves the circular queue and becomes fixed — never
/// compressed again. A seed is an extremely short description of the
/// original content, enough for the model to know what it was about.
const SEED_MAX_TOKENS: usize = 120;

/// Tolerance margin (percentage of MAX_CONTEXT_TOKENS) for the guard.
/// When a compressed result is within this margin, it is accepted.
/// The model never sees this value — it always targets the exact budget.
const TOLERANCE_PERCENT: u64 = 10;

/// Unified compression prompt.
const PROMPT_COMPRESSION: &str = "";

/// TF-IDF maximum document frequency filter: terms appearing in more than
/// this fraction of chunks are treated as stop-words and ignored
/// during deterministic compression (Layer 1).
const TFIDF_MAX_DF: f64 = 0.8;

/// MMR lambda balancing relevance and diversity in deterministic compression.
/// 1.0 = maximise relevance only (greedy), 0.0 = maximise diversity only.
const MMR_LAMBDA: f64 = 0.7;

/// Compression ratio for deterministic compression (Layer 1): keep roughly
/// this fraction of the input chunks after MMR selection.
const MMR_RATIO: f64 = 0.4;

/// Maximum token budget for the text sent to the synchronous LLM compression
/// call. Entries above this are pre-compressed deterministically first, so a
/// huge entry can never stall the agent loop with a giant round-trip.
const MAX_LLM_COMPRESSION_INPUT: usize = 4_000;

/// Maximum number of sentence-chunks processed per deterministic compression
/// pass. Larger inputs are split into batches to bound the O(n²) SVD/MMR
/// cost while preserving every chunk (nothing is lost, only compressed).
const DETERMINISTIC_MAX_CHUNKS: usize = 200;

/// Default empty slot used when resizing exhibitions.
const EMPTY_SLOT: Context = Context {
    hash_id: 0,
    lv: 0,
    user: String::new(),
    assistant: String::new(),
    tokens: 0,
    exhibition: 0,
};

/// A single entry in the compressed/exhibition context identified by a
/// bit-packed id: high 40 bits = exhibition slot, low 24 bits = level.
/// The model navigates levels with: `target = hash - lv + target_lv`.
#[derive(Clone, Serialize, Deserialize)]
pub struct Context {
    pub hash_id: u64,
    pub lv: u64,
    pub user: String,
    pub assistant: String,
    pub tokens: usize,
    pub exhibition: u64,
}

/// Indicates the role of a FreshContext chunk: who sent the message.
#[derive(Clone, Serialize, Deserialize)]
pub struct Role {
    pub assistant: Option<String>,
    pub user: Option<String>,
}

impl Role {
    /// Build a user Role.
    pub fn user(text: &str) -> Self {
        Self {
            assistant: None,
            user: Some(text.to_string()),
        }
    }

    /// Build an assistant Role.
    pub fn assistant(text: &str) -> Self {
        Self {
            assistant: Some(text.to_string()),
            user: None,
        }
    }

    /// Extract the text content regardless of which role holds it.
    pub fn text(&self) -> &str {
        self.assistant
            .as_deref()
            .or(self.user.as_deref())
            .unwrap_or_default()
    }

    /// Return the role label ("user" or "assistant").
    pub fn label(&self) -> &'static str {
        if self.assistant.is_some() {
            "assistant"
        } else if self.user.is_some() {
            "user"
        } else {
            ""
        }
    }
}

/// A chunk of fresh (not yet compressed) conversation context.
/// Each chunk carries its own checkpoint id and role for future agent reference.
#[derive(Clone, Serialize, Deserialize)]
pub struct FreshContext {
    pub checkpoints: u32,
    pub role: Role,
}

/// Serializable snapshot of [`ContextManager`] state for bincode persistence.
/// Every field is the minimum needed to reconstruct the full manager —
/// exhibitions are rebuilt on load; `layers` holds compressed parents that no
/// longer exist in the queue, so it is persisted as-is.
#[derive(Serialize, Deserialize)]
pub struct ContextManagerState {
    pub queue: VecDeque<Context>,
    pub fixed_contexts: Vec<Context>,
    /// Parent-content chain keyed by child hash. Not derivable from the queue
    /// after compression (parents are popped before being stored here), so it
    /// is persisted directly rather than rebuilt.
    pub layers: HashMap<u64, Vec<Context>>,
    pub buffer_chunks: Vec<FreshContext>,
    pub checkpoint_counter: u32,
    pub next_slot_id: u64,
}

/// Snapshot of context manager state for TUI display.
/// Cheap to compute — reads fields and estimates token counts without
/// building the full formatted string.
#[derive(Default, Clone, Debug)]
pub struct ContextDisplayInfo {
    /// Number of fresh buffer chunks (not yet compressed).
    pub fresh_chunks: usize,
    /// Number of compressed queue entries (in rotation).
    pub queue_entries: usize,
    /// Number of seed entries (permanently fixed).
    pub seed_count: usize,
    /// Number of non-empty exhibition slots.
    pub exhibitions: usize,
    /// Number of currently expanded entries.
    pub expanded: usize,
    /// Total estimated tokens of all content (buffer + queue + seeds).
    pub total_tokens: usize,
    /// Maximum token budget before compression triggers.
    pub max_budget: usize,
    /// Percentage of budget used (0-100).
    pub budget_pct: u8,
}

/// Orchestrates compression, expansion, and display of conversation context
/// through a circular queue. The inner [`Context`] entries are compressed
/// progressively and can be expanded on demand by the model via [`expand_slot`].
pub struct ContextManager {
    pub ctxt: Context,
    pub warning: bool,
    pub queue: VecDeque<Context>,
    pub fixed_contexts: Vec<Context>,
    pub connector: Connector,
    pub layers: HashMap<u64, Vec<Context>>,
    pub expand: Vec<u64>,
    pub next_slot_id: u64,
    pub exhibitions: Vec<Context>,

    // Cache for format_context output.
    rendered: Option<String>,
    dirty: bool,
    pub fresh: FreshContext,
    pub buffer_chunks: Vec<FreshContext>,
    pub checkpoint_counter: u32,

    /// Maximum compression retries before giving up.
    max_retries: u32,
}

// DISPLAY
impl ContextManager {
    /// Cheap snapshot of current context state for TUI display.
    /// Does NOT call format_context() — only reads fields and estimates
    /// raw token counts.
    pub fn display_info(&self) -> ContextDisplayInfo {
        let fresh_tokens: usize = self
            .buffer_chunks
            .iter()
            .map(|fc| estimate_tokens(fc.role.text()))
            .sum();
        let compressed_tokens: usize = self.queue.iter().map(|e| e.tokens).sum();
        let seed_tokens: usize = self.fixed_contexts.iter().map(|e| e.tokens).sum();
        let total = fresh_tokens + compressed_tokens + seed_tokens;
        let exhibitions = self.exhibitions.iter().filter(|e| e.hash_id != 0).count();
        ContextDisplayInfo {
            fresh_chunks: self.buffer_chunks.len(),
            queue_entries: self.queue.len(),
            seed_count: self.fixed_contexts.len(),
            exhibitions,
            expanded: self.expand.len(),
            total_tokens: total,
            max_budget: MAX_CONTEXT_TOKENS,
            budget_pct: if MAX_CONTEXT_TOKENS > 0 {
                ((total as f64 / MAX_CONTEXT_TOKENS as f64) * 100.0).min(100.0) as u8
            } else {
                0
            },
        }
    }
}

// ORCHESTRATION AND OPERATIONS
impl ContextManager {
    pub fn new(connector: Connector, max_retries: u32) -> Self {
        Self {
            ctxt: Context {
                hash_id: 0,
                lv: 0,
                user: String::new(),
                assistant: String::new(),
                tokens: 0,
                exhibition: 0,
            },
            warning: false,
            queue: VecDeque::new(),
            fixed_contexts: Vec::new(),
            connector,
            layers: HashMap::new(),
            expand: Vec::new(),
            next_slot_id: 0,
            exhibitions: Vec::new(),
            rendered: None,
            dirty: true,
            fresh: FreshContext {
                checkpoints: 0,
                role: Role {
                    assistant: None,
                    user: None,
                },
            },
            buffer_chunks: Vec::new(),
            checkpoint_counter: 0,
            max_retries,
        }
    }

    /// Run one iteration of the context manager:
    /// 1. Format and display the current context.
    /// 2. If total tokens exceed the budget, first drain the fresh buffer
    ///    (deterministic compression — no LLM round-trip), then rotate the
    ///    oldest queue entry (pop_front → compress → push_back).
    /// 3. Return the formatted context string.
    ///
    /// The buffer drain is what moves the TUI budget bar: in normal sessions
    /// the raw fresh chunks dominate the cost, so they are compressed into
    /// the queue as soon as the budget is exceeded.
    pub async fn run(&mut self) -> Option<String> {
        let context = self.format_context()?;

        if estimate_tokens(&context) >= MAX_CONTEXT_TOKENS {
            // Drain the over-budget fresh buffer deterministically first.
            if !self.buffer_chunks.is_empty() {
                let last_checkpoint = self
                    .buffer_chunks
                    .last()
                    .map(|c| c.checkpoints)
                    .unwrap_or(0);
                if let Err(e) = self.compress_fresh_up_to(last_checkpoint) {
                    log::debug!("run(): buffer drain failed: {e}");
                }
            }
            // Still over budget? Rotate the oldest queue entry.
            self.compress_oldest(None).await;
        }

        // Re-render so the returned context reflects the post-drain state
        // (the cache was invalidated by the compression above).
        self.format_context()
    }

    /// Return the current maximum retry limit for compression attempts.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Override the default retry limit for compression attempts.
    pub fn with_max_retries(&mut self, n: u32) -> &mut Self {
        self.max_retries = n;
        self
    }

    /// Add a fresh context chunk with an explicit role.
    /// Retains the individual chunk so `format_context` can display each
    /// checkpoint separately.
    pub fn add_buffer_context(&mut self, role: Role) {
        let breakpoint = self.checkpoint_counter + 1;
        self.checkpoint_counter = breakpoint;

        // Keep `fresh` updated with the latest checkpoint/role for quick reference.
        self.fresh = FreshContext {
            checkpoints: breakpoint,
            role: role.clone(),
        };

        // Retain the individual chunk so format_context can display
        // each checkpoint separately, oldest first.
        self.buffer_chunks.push(FreshContext {
            checkpoints: breakpoint,
            role,
        });

        self.invalidate_cache();
    }

    /// Mark the rendered cache as stale so the next call to
    /// [`format_context`](Self::format_context) rebuilds.
    fn invalidate_cache(&mut self) {
        self.dirty = true;
    }

    /// Create a new context entry, push it to the queue, and record it
    /// under its exhibition slot.
    ///
    /// Identifier is bit-packed (high bits = exhibition, low bits = level)
    /// so the model can navigate between layers with simple arithmetic:
    /// `target_hash = (current_hash - current_lv) + target_lv`.
    ///
    /// `prev_hash` is accepted for compatibility but not used — the
    /// parent-child relationship is maintained in the `layers` map.
    ///
    /// The compressed text is placed in `assistant`; `user` remains empty
    /// since compressed content is an assistant-style summary.
    pub(crate) fn build_context(
        &mut self,
        text: String,
        prev_hash: Option<u64>,
        lv: u64,
        tokens: usize,
        exhibition: Option<u64>,
    ) -> &Self {
        let exhibition = exhibition.unwrap_or_else(|| {
            self.next_slot_id += 1;
            self.next_slot_id
        });

        let hash_id = (exhibition << 24) | lv;
        let _ = prev_hash;

        let entry = Context {
            hash_id,
            lv,
            user: String::new(),
            assistant: text,
            tokens,
            exhibition,
        };
        let idx = entry.exhibition as usize;

        self.queue.push_back(entry.clone());
        if self.exhibitions.len() <= idx {
            self.exhibitions.resize(idx + 1, EMPTY_SLOT);
        }
        self.exhibitions[idx] = entry;
        self.invalidate_cache();
        self
    }

    /// Push a pre-built [`Context`] entry directly to the back of the queue.
    /// Unlike [`build_context`](Self::build_context), this does not compute
    /// hashes or manage exhibition slots — it inserts the entry as-is.
    #[allow(dead_code)]
    pub(crate) fn enqueue(&mut self, entry: Context) -> &Self {
        self.queue.push_back(entry);
        self.invalidate_cache();
        self
    }

    /// Rotate the oldest queue entry through compression and push it to
    /// the back. Delegates to [`compress_if_needed`] for the guard loop.
    ///
    /// When an entry reaches seed size (≤ [`SEED_MAX_TOKENS`]) it leaves
    /// the queue and becomes fixed — never compressed again.
    pub(crate) async fn compress_oldest(&mut self, raw_content: Option<&str>) -> &Self {
        if let Some(raw) = raw_content {
            let tokens = estimate_tokens(raw);
            self.build_context(raw.to_string(), None, 1, tokens, None);
            return self;
        }

        let entry = match self.queue.pop_front() {
            Some(e) => e,
            None => return self,
        };

        // Already at seed size — move straight to fixed, no more compression.
        if entry.tokens <= SEED_MAX_TOKENS {
            self.fixed_contexts.push(entry);
            self.invalidate_cache();
            return self;
        }

        let summary = match self.compress_if_needed(&entry).await {
            Some(s) => s,
            None => {
                log::error!(
                    "[context_manager] compress_if_needed failed (hash={})",
                    entry.hash_id
                );
                // Never lose context: restore the entry so the next tick can
                // retry compression instead of silently dropping it.
                self.queue.push_front(entry);
                self.invalidate_cache();
                return self;
            }
        };

        let tokens = estimate_tokens(&summary);

        // Compressed to seed size — build the exhibition entry and move it
        // to fixed. It leaves the circular queue permanently.
        if tokens <= SEED_MAX_TOKENS {
            let hash_id = (entry.exhibition << 24) | (entry.lv + 1);
            let seed = Context {
                hash_id,
                lv: entry.lv + 1,
                user: String::new(),
                assistant: summary,
                tokens,
                exhibition: entry.exhibition,
            };
            let idx = seed.exhibition as usize;
            if self.exhibitions.len() <= idx {
                self.exhibitions.resize(idx + 1, EMPTY_SLOT);
            }
            self.exhibitions[idx] = seed.clone();
            self.fixed_contexts.push(seed);
            self.layers.insert(hash_id, vec![entry]);
            self.invalidate_cache();
            return self;
        }

        // Still above seed size — normal rotation back into the queue.
        self.build_context(
            summary,
            Some(entry.hash_id),
            entry.lv + 1,
            tokens,
            Some(entry.exhibition),
        );

        if let Some(child) = self.queue.back() {
            self.layers.insert(child.hash_id, vec![entry]);
        }
        self
    }

    /// Compress an entry with guard verification and retry loop.
    ///
    /// The LLM path is best-effort: when the call fails or the retries are
    /// exhausted without an under-budget result, the entry is still compressed
    /// with the deterministic pipeline — it is never dropped.
    async fn compress_if_needed(&self, entry: &Context) -> Option<String> {
        let tolerance = MAX_CONTEXT_TOKENS * (TOLERANCE_PERCENT as usize) / 100;
        let mut current_text = if !entry.assistant.is_empty() {
            entry.assistant.clone()
        } else {
            entry.user.clone()
        };
        let mut current_tokens = entry.tokens;

        // Bound the synchronous LLM call: pre-compress deterministically
        // anything above the call budget so a huge entry cannot stall the
        // agent loop with a giant round-trip.
        if current_tokens > MAX_LLM_COMPRESSION_INPUT {
            current_text = self.deterministic_compression(&current_text);
            current_tokens = estimate_tokens(&current_text);
        }

        for _ in 0..self.max_retries {
            let reduction_needed = current_tokens.saturating_sub(MAX_CONTEXT_TOKENS);

            let summary = match self
                .compress_via_llm(&current_text, MAX_CONTEXT_TOKENS, reduction_needed)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    log::error!("[context_manager] compression failed: {e}");
                    // LLM unavailable (MissingApiKey, timeout, ...) — fall back
                    // to the deterministic pipeline. Nothing is dropped.
                    return Some(self.deterministic_compression(&current_text));
                }
            };

            let result_tokens = estimate_tokens(&summary);

            if result_tokens <= MAX_CONTEXT_TOKENS {
                return Some(summary);
            }
            if result_tokens <= MAX_CONTEXT_TOKENS + tolerance {
                return Some(self.deterministic_compression(&summary));
            }

            current_text = summary;
            current_tokens = result_tokens;
        }
        // Retries exhausted without an under-budget result — the deterministic
        // fallback keeps the entry compressed and never loses it.
        Some(self.deterministic_compression(&current_text))
    }

    /// Compress a context entry via the LLM with an informative prompt.
    async fn compress_via_llm(
        &self,
        text: &str,
        max_tokens: usize,
        reduction_needed: usize,
    ) -> Result<String, String> {
        let prompt = format!(
            "## Compression Task\n\
             Maximum context budget: {max_tokens} tokens.\n\
             Current block size: {current} tokens.\n\
             Target reduction: reduce by at least {reduction} tokens.\n\
             \n\
             {base}\n",
            max_tokens = max_tokens,
            current = estimate_tokens(text),
            reduction = reduction_needed,
            base = PROMPT_COMPRESSION,
        );
        let output = self
            .connector
            .chat_with_system(text, &prompt)
            .await
            .map_err(|e| e.to_string())?;

        Ok(output.message().to_string())
    }

    /// Helper: get the text from a FreshContext regardless of its role.
    fn fresh_text(fc: &FreshContext) -> &str {
        fc.role.text()
    }

    /// Compress fresh buffer chunks from checkpoint 1 up to the given `checkpoint`.
    ///
    /// The model triggers this by passing the checkpoint number visible in the
    /// formatted context — e.g. `(checkpoint 4)`. After compression the
    /// remaining chunks are **re-indexed** so the new checkpoint 1 is the first
    /// chunk that was *not* compressed, keeping the visual numbering contiguous.
    ///
    /// Returns confirmation message on success, or an error string on failure.
    pub fn compress_fresh_up_to(&mut self, checkpoint: u32) -> Result<String, String> {
        if checkpoint == 0 {
            return Err("Checkpoint must be greater than 0.".to_string());
        }
        if self.buffer_chunks.is_empty() {
            return Err("No fresh context to compress.".to_string());
        }

        // Find the first chunk whose checkpoint exceeds the target.
        // All chunks before it (0..idx) get compressed together.
        let idx = self
            .buffer_chunks
            .iter()
            .position(|c| c.checkpoints > checkpoint)
            .unwrap_or(self.buffer_chunks.len());

        if idx == 0 {
            return Err(format!(
                "No fresh context chunks with checkpoint ≤ {checkpoint}."
            ));
        }

        // Concatenate the raw text of all consumed chunks (regardless of role).
        let combined: String = self.buffer_chunks[..idx]
            .iter()
            .map(Self::fresh_text)
            .collect();

        // Run deterministic compression (Layer 1) on the combined text.
        let compressed = self.deterministic_compression(&combined);
        let tokens = estimate_tokens(&compressed);

        // Build a Context entry (LV = 1) — pushes to queue + exhibitions.
        self.build_context(compressed, None, 1, tokens, None);

        // Remove the consumed chunks from the buffer.
        self.buffer_chunks.drain(..idx);

        // Re-index remaining chunks so they start fresh from 1.
        for (i, chunk) in self.buffer_chunks.iter_mut().enumerate() {
            chunk.checkpoints = (i + 1) as u32;
        }

        // Rebuild `fresh` from what is left.
        if let Some(last) = self.buffer_chunks.last() {
            self.fresh = FreshContext {
                checkpoints: last.checkpoints,
                role: last.role.clone(),
            };
        } else {
            self.fresh = FreshContext {
                checkpoints: 0,
                role: Role {
                    assistant: None,
                    user: None,
                },
            };
        }
        self.checkpoint_counter = self.fresh.checkpoints;

        self.invalidate_cache();

        let chunk_count = idx;
        let compressed_tokens = estimate_tokens(&combined);
        Ok(format!(
            "Compressed {chunk_count} fresh chunk(s) ({compressed_tokens} tokens) up to checkpoint {checkpoint}."
        ))
    }

    /// Deterministic compression (Layer1) via TF-IDF → LSA → MMR.
    /// Splits text into sentence-like chunks using text-splitter, ranks them
    /// by relevance + diversity, and returns the most informative subset.
    ///
    /// Very large inputs are processed in bounded batches so the O(n²)
    /// SVD/MMR cost cannot stall the agent loop; every chunk still passes
    /// through the pipeline (nothing is lost).
    fn deterministic_compression(&self, ctxt: &str) -> String {
        if estimate_tokens(ctxt) <= MAX_CONTEXT_TOKENS {
            return deterministic_compress(ctxt, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO);
        }

        let splitter = TextSplitter::new(200);
        let chunks: Vec<&str> = splitter.chunks(ctxt).collect();
        chunks
            .chunks(DETERMINISTIC_MAX_CHUNKS)
            .map(|batch| {
                let text = batch.concat();
                deterministic_compress(&text, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO)
            })
            .collect()
    }

    /// Mark a hash for expansion so the next [`format_context`] call
    /// shows the parent content in place of the compressed version.
    #[allow(dead_code)]
    pub(crate) fn expand_slot(&mut self, hash_id: u64) -> &Self {
        self.expand.push(hash_id);
        self.invalidate_cache();
        self
    }

    /// Expand a specific level by passing the visible hash and the desired
    /// level. Resolves the target hash internally — the model never needs
    /// to do arithmetic.
    ///
    /// `hash` is any visible hash for this exhibition slot; `target_lv` is
    /// the level to expand to (e.g. `1` for the original content).
    ///
    /// Returns `Ok(self)` on success, or `Err(message)` with a model-facing
    /// explanation when the entry or level does not exist.
    pub fn expand_to(&mut self, hash: u64, target_lv: u64) -> Result<&Self, String> {
        let exhibition_id = (hash >> 24) as usize;
        if exhibition_id >= self.exhibitions.len() {
            return Err(format!(
                "Context entry #{hash} not found. Use a valid hash from the context view."
            ));
        }
        let entry = &self.exhibitions[exhibition_id];
        if entry.hash_id == 0 {
            return Err(format!(
                "Context entry #{hash} not found. Use a valid hash from the context view."
            ));
        }
        if target_lv > entry.lv {
            return Err(format!(
                "Level {target_lv} does not exist for entry #{hash}. \
                 The highest level available is {}. Use a level ≤ {} to expand.",
                entry.lv, entry.lv
            ));
        }
        let target_hash = (hash & !0xFFFFFF) | target_lv;
        self.expand.push(target_hash);
        self.invalidate_cache();
        Ok(self)
    }

    /// Return to the natural compressed view. Clears every expansion mark
    /// and invalidates the cache so the next [`format_context`] renders
    /// the compressed state — as if no expand had ever been called.
    ///
    /// Intended for external callers (e.g. harness core after each agent
    /// loop) to guarantee the display reverts to the efficient layout.
    pub fn collapse_context(&mut self) -> &Self {
        self.expand.clear();
        self.invalidate_cache();
        self
    }

    /// Produce a serializable snapshot of the current state.
    pub fn save_state(&self) -> ContextManagerState {
        ContextManagerState {
            queue: self.queue.clone(),
            fixed_contexts: self.fixed_contexts.clone(),
            layers: self.layers.clone(),
            buffer_chunks: self.buffer_chunks.clone(),
            checkpoint_counter: self.checkpoint_counter,
            next_slot_id: self.next_slot_id,
        }
    }

    /// Rebuild exhibitions after loading from state. The `layers` map is
    /// restored directly from the snapshot — compressed parents are popped
    /// from the queue before being stored in `layers`, so they cannot be
    /// reconstructed from queue relationships.
    fn rebuild_internals(&mut self) {
        self.exhibitions.clear();
        for entry in self.queue.iter().chain(self.fixed_contexts.iter()) {
            let idx = entry.exhibition as usize;
            if self.exhibitions.len() <= idx {
                self.exhibitions.resize(idx + 1, EMPTY_SLOT);
            }
            self.exhibitions[idx] = entry.clone();
        }

        self.layers.clear();
        let mut by_exhibition: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, e) in self.queue.iter().enumerate() {
            by_exhibition.entry(e.exhibition).or_default().push(i);
        }
        for indices in by_exhibition.values() {
            let mut pairs: Vec<(u64, usize)> =
                indices.iter().map(|&i| (self.queue[i].lv, i)).collect();
            pairs.sort_by_key(|(lv, _)| *lv);
            for w in pairs.windows(2) {
                let (_, parent_idx) = w[0];
                let (_, child_idx) = w[1];
                let child = &self.queue[child_idx];
                let parent = self.queue[parent_idx].clone();
                self.layers.insert(child.hash_id, vec![parent]);
            }
        }
    }

    /// Restore the full manager state from a previously saved snapshot.
    /// Rebuilds exhibitions and restores the layers map.
    pub fn restore_state(&mut self, state: &ContextManagerState) {
        self.queue = state.queue.clone();
        self.fixed_contexts = state.fixed_contexts.clone();
        self.buffer_chunks = state.buffer_chunks.clone();
        self.checkpoint_counter = state.checkpoint_counter;
        self.next_slot_id = state.next_slot_id;
        self.rebuild_internals();
        self.layers = state.layers.clone();
        self.invalidate_cache();
    }

    /// Build the formatted context display.
    ///
    /// The output is cached and only rebuilt when state changes.
    /// Shows buffer chunks with their role (user/assistant) and exhibitions
    /// with their compressed content.
    pub fn format_context(&mut self) -> Option<String> {
        if !self.dirty
            && let Some(cached) = &self.rendered
        {
            return Some(cached.clone());
        }

        // 1. Resolve each expand target: extract exhibition slot and walk
        //    the layers chain until we reach the requested level.
        let mut expanded_slots = self.resolve_expanded_slots();

        // 2. Buffer context (fresh, not yet summarized).
        //    Oldest buffer entry first → newest last.
        let mut result = String::new();
        if !self.buffer_chunks.is_empty() {
            result.push_str("## Full\n\n");
            for entry in &self.buffer_chunks {
                let role_label = entry.role.label();
                let text = entry.role.text();
                result.push_str(&format!(
                    "(checkpoint {}) {}:\n{}\n\n",
                    entry.checkpoints, role_label, text
                ));
            }
        }

        // 3. Render exhibitions in order, replacing compressed entries
        //    with expanded versions where applicable.
        let has_exhibitions = self
            .exhibitions
            .iter()
            .any(|exh| exh.hash_id != 0 || !exh.assistant.is_empty() || !exh.user.is_empty());
        if has_exhibitions {
            result.push_str("## SUMMARY\n\n");
            for (i, exh) in self.exhibitions.iter().enumerate() {
                if exh.hash_id == 0 && exh.assistant.is_empty() && exh.user.is_empty() {
                    continue;
                }
                if let Some((text, lv, hash)) = expanded_slots.remove(&i) {
                    result.push_str(&format!("({hash}, {lv}) [EXPANDED]\n{text}\n\n"));
                } else {
                    // Show content directly without role prefix
                    let text = if !exh.assistant.is_empty() {
                        &exh.assistant
                    } else {
                        &exh.user
                    };
                    result.push_str(&format!(
                        "({hash}, {lv})\n{text}\n\n",
                        hash = exh.hash_id,
                        lv = exh.lv,
                    ));
                }
            }
        }

        self.rendered = Some(result.clone());
        self.dirty = false;
        Some(result)
    }

    /// Resolve each expand target: extract the exhibition slot and walk the
    /// layers chain until the requested level is reached.
    fn resolve_expanded_slots(&self) -> HashMap<usize, (String, u64, u64)> {
        let mut expanded_slots: HashMap<usize, (String, u64, u64)> = HashMap::new();

        for &target in &self.expand {
            let idx = (target >> 24) as usize;
            if idx >= self.exhibitions.len() {
                continue;
            }
            let exh = &self.exhibitions[idx];
            if exh.hash_id == 0 && exh.assistant.is_empty() && exh.user.is_empty() {
                continue;
            }

            let target_lv = target & 0xFFFFFF;
            if target_lv >= exh.lv {
                continue;
            }

            let mut walk_hash = exh.hash_id;
            let mut walk_lv = exh.lv;
            let mut walk_text = if !exh.assistant.is_empty() {
                exh.assistant.clone()
            } else {
                exh.user.clone()
            };

            while walk_lv > target_lv {
                match self.layers.get(&walk_hash).and_then(|v| v.first()) {
                    Some(parent) => {
                        walk_hash = parent.hash_id;
                        walk_lv = parent.lv;
                        walk_text = if !parent.assistant.is_empty() {
                            parent.assistant.clone()
                        } else {
                            parent.user.clone()
                        };
                    }
                    None => break,
                }
            }
            expanded_slots.insert(idx, (walk_text, walk_lv, target));
        }

        expanded_slots
    }

    /// Render only the compressed/exhibition portion of the context — the
    /// hashes and summaries the model needs for `expand_context` and
    /// `force_compress`.
    ///
    /// The raw buffer is deliberately excluded: fresh turns are delivered via
    /// the messages array, so including them here would duplicate every turn
    /// in the system prompt.
    ///
    /// Returns `None` when there is nothing compressed to show.
    pub fn format_compressed_context(&mut self) -> Option<String> {
        let mut expanded_slots = self.resolve_expanded_slots();

        let has_exhibitions = self
            .exhibitions
            .iter()
            .any(|exh| exh.hash_id != 0 || !exh.assistant.is_empty() || !exh.user.is_empty());
        if !has_exhibitions {
            return None;
        }

        let mut result = String::new();
        result.push_str("## SUMMARY\n\n");
        for (i, exh) in self.exhibitions.iter().enumerate() {
            if exh.hash_id == 0 && exh.assistant.is_empty() && exh.user.is_empty() {
                continue;
            }
            if let Some((text, lv, hash)) = expanded_slots.remove(&i) {
                result.push_str(&format!("({hash}, {lv}) [EXPANDED]\n{text}\n\n"));
            } else {
                let text = if !exh.assistant.is_empty() {
                    &exh.assistant
                } else {
                    &exh.user
                };
                result.push_str(&format!(
                    "({hash}, {lv})\n{text}\n\n",
                    hash = exh.hash_id,
                    lv = exh.lv,
                ));
            }
        }
        Some(result)
    }
}
