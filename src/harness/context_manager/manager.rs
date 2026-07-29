//! Layered context management system that progressively compresses conversation
//! history into a circular queue instead of discarding it. Models can seamlessly
//! navigate between compressed and expanded context through tools.
//!
//! Nothing is lost—only compressed, and always navigable.
//!
use crate::util::token_counter::estimate_tokens;
use cosh_sdk::connector::Connector;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

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

/// Default retry limit for compression attempts.
const DEFAULT_MAX_RETRIES: u32 = 3;

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

/// Default empty slot used when resizing exhibitions.
const EMPTY_SLOT: Context = Context {
    hash_id: 0,
    lv: 0,
    ctxt: String::new(),
    tokens: 0,
    exhibition: 0,
};

/// A single entry in the compressed/exhibition context identified by a
/// bit-packed id: high 40 bits = exhibition slot, low 24 bits = level.
/// The model navigates levels with: `target = hash - lv + target_lv`.
#[derive(Clone)]
pub struct Context {
    pub hash_id: u64,
    pub lv: u64,
    pub ctxt: String,
    pub tokens: usize,
    pub exhibition: u64,
}

/// A chunk of fresh (not yet compressed) conversation context.
/// Each chunk carries its own checkpoint id for future agent reference.
pub struct FreshContext {
    pub checkpoints: u32,
    pub ctxt: String,
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

    // Cache for format_context output - interior mutability so the
    // method can remain &self while lazily updating on changes.
    rendered: RefCell<Option<String>>,
    dirty: Cell<bool>,
    pub fresh: FreshContext,
    pub buffer_chunks: Vec<FreshContext>,
    pub checkpoint_counter: u32,

    /// Maximum compression retries before giving up.
    max_retries: u32,
}

impl ContextManager {
    pub fn new(connector: Connector) -> Self {
        Self {
            ctxt: Context {
                hash_id: 0,
                lv: 0,
                ctxt: String::new(),
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
            rendered: RefCell::new(None),
            dirty: Cell::new(true),
            fresh: FreshContext {
                checkpoints: 0,
                ctxt: String::new(),
            },
            buffer_chunks: Vec::new(),
            checkpoint_counter: 0,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }

    /// Run one iteration of the context manager:
    /// 1. Format and display the current context.
    /// 2. If total tokens exceed the budget, compress the oldest queue entry
    ///    (pop_front → compress → push_back — circular queue rotation).
    /// 3. Return the formatted context string.
    pub async fn run(&mut self) -> Option<String> {
        let context = self.format_context()?;

        if estimate_tokens(&context) >= MAX_CONTEXT_TOKENS {
            self.compress_oldest(None).await;
        }

        Some(context)
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

    pub fn add_buffer_context(&mut self, ctxt: &str) {
        // Accumulate into the single text field for token accounting.
        self.fresh.ctxt.push_str(ctxt);

        let breakpoint = self.checkpoint_counter + 1;
        self.fresh.checkpoints = breakpoint;
        self.checkpoint_counter = breakpoint;

        // Retain the individual chunk so format_context can display
        // each checkpoint separately, oldest first.
        self.buffer_chunks.push(FreshContext {
            checkpoints: breakpoint,
            ctxt: ctxt.to_string(),
        });

        self.invalidate_cache();
    }

    /// Mark the rendered cache as stale so the next call to
    /// [`format_context`](Self::format_context) rebuilds.
    fn invalidate_cache(&mut self) {
        self.dirty.set(true);
    }

    /// Restore a previously saved queue and fixed contexts from a prior
    /// session state.
    pub fn restore(&mut self, queue: VecDeque<Context>, fixed_contexts: Vec<Context>) -> &Self {
        self.queue = queue;
        self.fixed_contexts = fixed_contexts;
        self.invalidate_cache();
        self
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
    pub fn build_context(
        &mut self,
        ctxt: String,
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
            ctxt,
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
    pub fn enqueue(&mut self, ctxt: Context) -> &Self {
        self.queue.push_back(ctxt);
        self.invalidate_cache();
        self
    }

    /// Rotate the oldest queue entry through compression and push it to
    /// the back. Delegates to [`compress_if_needed`] for the guard loop.
    ///
    /// When an entry reaches seed size (≤ [`SEED_MAX_TOKENS`]) it leaves
    /// the queue and becomes fixed — never compressed again.
    pub async fn compress_oldest(&mut self, raw_content: Option<&str>) -> &Self {
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
                ctxt: summary,
                tokens,
                exhibition: entry.exhibition,
            };
            let idx = seed.exhibition as usize;
            if self.exhibitions.len() <= idx {
                self.exhibitions.resize(idx + 1, EMPTY_SLOT);
            }
            self.exhibitions[idx] = seed.clone();
            self.fixed_contexts.push(seed);
            self.layers.insert(hash_id as u64, vec![entry]);
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
            self.layers.insert(child.hash_id as u64, vec![entry]);
        }
        self
    }

    /// Compress an entry with guard verification and retry loop.
    async fn compress_if_needed(&self, entry: &Context) -> Option<String> {
        let tolerance = MAX_CONTEXT_TOKENS * (TOLERANCE_PERCENT as usize) / 100;
        let mut current = entry.clone();

        for _ in 0..self.max_retries {
            let reduction_needed = current.tokens.saturating_sub(MAX_CONTEXT_TOKENS);

            let summary = self
                .compress_via_llm(&current, MAX_CONTEXT_TOKENS, reduction_needed)
                .await
                .map_err(|e| log::error!("[context_manager] compression failed: {e}"))
                .ok()?;

            let result_tokens = estimate_tokens(&summary);

            if result_tokens <= MAX_CONTEXT_TOKENS {
                return Some(summary);
            }
            if result_tokens <= MAX_CONTEXT_TOKENS + tolerance {
                return Some(self.deterministic_compression(&summary));
            }

            current.ctxt = summary;
            current.tokens = result_tokens;
        }
        None
    }

    /// Compress a context entry via the LLM with an informative prompt.
    async fn compress_via_llm(
        &self,
        ctxt: &Context,
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
            current = ctxt.tokens,
            reduction = reduction_needed,
            base = PROMPT_COMPRESSION,
        );
        let output = self
            .connector
            .chat_with_system(&ctxt.ctxt, &prompt)
            .await
            .map_err(|e| e.to_string())?;

        Ok(output.message().to_string())
    }

    /// Compress fresh buffer chunks from checkpoint 1 up to the given `checkpoint`.
    ///
    /// The model triggers this by passing the checkpoint number visible in the
    /// formatted context — e.g. `[BUFFER] (checkpoint 4)`. After compression the
    /// remaining chunks are **re-indexed** so the new checkpoint 1 is the first
    /// chunk that was *not* compressed, keeping the visual numbering contiguous.
    ///
    /// Returns the combined raw text of the consumed chunks (before compression)
    /// so the caller can inspect what was folded in, or `None` when there is
    /// nothing to compress (empty buffer, zero checkpoint, or all checkpoints
    /// are already past the target).
    pub fn compress_fresh_up_to(&mut self, checkpoint: u32) -> Option<String> {
        if checkpoint == 0 || self.buffer_chunks.is_empty() {
            return None;
        }

        // Find the first chunk whose checkpoint exceeds the target.
        // All chunks before it (0..idx) get compressed together.
        let idx = self
            .buffer_chunks
            .iter()
            .position(|c| c.checkpoints > checkpoint)
            .unwrap_or(self.buffer_chunks.len());

        if idx == 0 {
            return None; // no chunk has a checkpoint ≤ target
        }

        // Concatenate the raw text of all consumed chunks.
        let combined: String = self.buffer_chunks[..idx]
            .iter()
            .map(|c| c.ctxt.as_str())
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

        // Rebuild the aggregated fresh context from what is left.
        self.fresh.ctxt = self.buffer_chunks.iter().map(|c| c.ctxt.as_str()).collect();
        self.fresh.checkpoints = self
            .buffer_chunks
            .last()
            .map(|c| c.checkpoints)
            .unwrap_or(0);
        self.checkpoint_counter = self.fresh.checkpoints;

        self.invalidate_cache();
        Some(combined)
    }

    /// Deterministic compression (Layer1) via TF-IDF → LSA → MMR.
    /// Splits text into sentence-like chunks using text-splitter, ranks them
    /// by relevance + diversity, and returns the most informative subset.
    pub fn deterministic_compression(&self, ctxt: &str) -> String {
        deterministic_compress(ctxt, false, TFIDF_MAX_DF, MMR_LAMBDA, MMR_RATIO)
    }

    /// Mark a hash for expansion so the next [`format_context`] call
    /// shows the parent content in place of the compressed version.
    pub fn expand_slot(&mut self, hash_id: u64) -> &Self {
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
    pub fn expand_to(&mut self, hash: u64, target_lv: u64) -> &Self {
        let target_hash = (hash & !0xFFFFFF) | target_lv;
        self.expand.push(target_hash);
        self.invalidate_cache();
        self
    }

    /// Clear all expansion marks.
    pub fn clear_expand(&mut self) -> &Self {
        self.expand.clear();
        self.invalidate_cache();
        self
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

    /// Build the formatted context display.
    ///
    /// The output is cached and only rebuilt when state changes.
    pub fn format_context(&self) -> Option<String> {
        if !self.dirty.get() {
            if let Some(cached) = self.rendered.borrow().as_ref() {
                return Some(cached.clone());
            }
        }

        // 1. Resolve each expand target: extract exhibition slot and walk
        //    the layers chain until we reach the requested level.
        let mut expanded_slots: HashMap<usize, (String, u64, u64)> = HashMap::new();

        for &target in &self.expand {
            let idx = (target >> 24) as usize;
            if idx >= self.exhibitions.len() {
                continue;
            }
            let exh = &self.exhibitions[idx];
            if exh.hash_id == 0 && exh.ctxt.is_empty() {
                continue;
            }

            let target_lv = target & 0xFFFFFF;
            if target_lv >= exh.lv {
                continue;
            }

            let mut walk_hash = exh.hash_id;
            let mut walk_lv = exh.lv;
            let mut walk_ctxt = exh.ctxt.clone();

            while walk_lv > target_lv {
                match self.layers.get(&(walk_hash as u64)).and_then(|v| v.first()) {
                    Some(parent) => {
                        walk_hash = parent.hash_id;
                        walk_lv = parent.lv;
                        walk_ctxt = parent.ctxt.clone();
                    }
                    None => break,
                }
            }
            expanded_slots.insert(idx, (walk_ctxt, walk_lv, target));
        }

        // 2. Prepend buffer context (fresh, not yet summarized).
        //    Oldest buffer entry first → newest last, all before
        //    exhibitions so the most-recent raw content appears first.
        let mut result = String::new();
        for entry in &self.buffer_chunks {
            result.push_str(&format!(
                "[BUFFER] (checkpoint {})\n{}\n\n",
                entry.checkpoints, entry.ctxt
            ));
        }

        // 3. Render exhibitions in order, replacing compressed entries
        //    with expanded versions where applicable.
        for (i, exh) in self.exhibitions.iter().enumerate() {
            if exh.hash_id == 0 && exh.ctxt.is_empty() {
                continue;
            }
            if let Some((ctxt, lv, hash)) = expanded_slots.remove(&i) {
                result.push_str(&format!("({hash}, {lv}) [EXPANDED]\n{ctxt}\n\n"));
            } else {
                result.push_str(&format!(
                    "({hash}, {lv})\n{content}\n\n",
                    hash = exh.hash_id,
                    lv = exh.lv,
                    content = exh.ctxt,
                ));
            }
        }

        self.rendered.replace(Some(result.clone()));
        self.dirty.set(false);
        Some(result)
    }
}
