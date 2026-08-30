//! Split-and-concatenate: the context-window contingency, driven by the
//! harness.
//!
//! When the single-shot LLM compaction itself overflows the model window, the
//! timeline is summarized in SEQUENTIAL CHUNKS: each chunk continues where the
//! previous one ended, and the summaries are concatenated into ONE final
//! anchored summary. The implementation lives on [`ContextManager`] (this
//! module is a descendant of the context module, so it can reach its private
//! fields); the timeline is NEVER touched until
//! [`ContextManager::commit_split`] replaces it atomically.

use super::summarize::{SPLIT_SUMMARIZER_SYSTEM, build_split_prompt, serialize_item};
use super::{ContextItem, ContextManager};
use serde::{Deserialize, Serialize};

/// The final anchor buffer must stay at or below this fraction of the model's
/// window (the "ceiling" — the max capacity of the concatenation buffer).
pub(super) const SPLIT_BUFFER_MAX_PCT: f64 = 0.40;

/// Fraction of the buffer ceiling at which the limit message starts being
/// sent to the summarizer. Below it the model is left to act naturally (a
/// long-session summary rarely approaches the ceiling); only when the
/// accumulation becomes concerning do we ask it to be more terse.
pub(super) const SPLIT_WARN_PCT: f64 = 0.80;

/// How many trailing chars of the previous chunk summary are carried into the
/// next chunk as the continuity snippet, so the final buffer reads as one
/// continuous summary (no visible seam).
pub(super) const SPLIT_CONTINUITY_CHARS: usize = 1200;

/// Fixed token overhead reserved per chunk call: the summarizer system prompt,
/// the split template/instructions, the continuity snippet, the metadata lines
/// and the model's output headroom. The serialized chunk itself is sized to
/// `window - overhead` so the whole request fits the window. The reserve is
/// capped at half the window (see [`ContextManager::split_next_chunk`]) so a
/// small window (a small fallback model) keeps a usable chunk budget instead
/// of degrading to one item per chunk.
pub(super) const SPLIT_CALL_OVERHEAD: usize = 8000;

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
    /// projection does not depend on re-estimation after a restart). An
    /// ADDITIVE estimate — per-part token counts, not the concatenated
    /// text's — so it may run a few tokens above the exact value; the
    /// direction is conservative (warns slightly earlier) and the commit
    /// re-estimates the whole buffer, so it never depends on this estimate.
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

// The impl block continues here: these methods are the split lifecycle
// (begin → next_chunk/advance → commit/abort).

impl ContextManager {
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
        // Incremental accounting: only the NEW summary is tokenized (the
        // joiner counted too) — re-encoding the whole buffer per chunk would
        // be O(total)² across a long split.
        let summary_tokens = self.encoding.estimate(summary);
        if split.buffer.is_empty() {
            split.buffer_tokens = summary_tokens;
        } else {
            split.buffer.push('\n');
            split.buffer_tokens = split
                .buffer_tokens
                .saturating_add(self.encoding.estimate("\n"))
                .saturating_add(summary_tokens);
        }
        split.buffer.push_str(summary);

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
    /// [`Self::apply_llm_summary`] reports: the split is driven by a
    /// model-window overflow, so its anchor only needs to fit that window — an
    /// anchor well under 80% of the budget is still a valid commit. On failure
    /// (the anchor is still too big) the timeline is left untouched and the
    /// caller records the provider as stuck and notifies.
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
}
