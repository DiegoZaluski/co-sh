//! Hierarchical MapReduce contingency for checkpoint construction.
//!
//! The staging state stores references and derived summaries only. Raw source
//! text is regenerated from [`ContextManager`]'s immutable item history for
//! each request, so MapReduce never becomes another authoritative transcript.

use super::summarize::{
    MAP_SUMMARIZER_SYSTEM, REDUCE_SUMMARIZER_SYSTEM, VALIDATOR_SYSTEM, build_correction_prompt,
    build_map_prompt, build_reduce_prompt, build_validation_prompt, serialize_item,
};
use super::{
    COMPACT_PCT, ContextItemRange, ContextManager, LlmCompactionRequest, coalesce_ranges,
    merge_ranges,
};
use serde::{Deserialize, Serialize};

const MAP_CALL_OVERHEAD: usize = 8_000;
const REDUCE_CALL_OVERHEAD: usize = 6_000;
const MAX_REREAD_RANGES: usize = 3;
const MAX_REPARTITIONS: u8 = 3;
const MAX_REDUCTION_LEVELS: usize = 8;
const MAX_CORRECTIONS: u8 = 2;
const MAP_REDUCE_VERSION: u8 = 1;

const fn map_reduce_version() -> u8 {
    MAP_REDUCE_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentSlice {
    pub item_id: u64,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MapSegment {
    pub ordinal: usize,
    pub slices: Vec<SegmentSlice>,
    pub covered_ranges: Vec<ContextItemRange>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryNode {
    pub covered_ranges: Vec<ContextItemRange>,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MapReducePhase {
    #[default]
    Mapping,
    Reducing,
    Validating,
    Correcting,
    Ready,
}

/// Persisted, resumable MapReduce staging. All text fields are derived
/// summaries; source content is addressed by item IDs and byte slices.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MapReduceState {
    #[serde(default = "map_reduce_version")]
    pub version: u8,
    pub window: usize,
    #[serde(default)]
    pub model: Option<String>,
    pub source_item_ids: Vec<u64>,
    /// Fingerprints of the frozen source projection. Item IDs can survive a
    /// revert or an item replacement; stale staging must never cover changed
    /// content under the same ID.
    #[serde(default)]
    pub source_fingerprints: Vec<(u64, u64)>,
    pub segments: Vec<MapSegment>,
    #[serde(default)]
    pub phase: MapReducePhase,
    #[serde(default)]
    pub level: usize,
    #[serde(default)]
    pub nodes: Vec<SummaryNode>,
    #[serde(default)]
    pub next_nodes: Vec<SummaryNode>,
    #[serde(default)]
    pub reduce_cursor: usize,
    #[serde(default)]
    pub candidate: Option<String>,
    #[serde(default)]
    pub validation_cursor: usize,
    #[serde(default)]
    pub audits: Vec<String>,
    #[serde(default)]
    pub final_summary: Option<String>,
    /// Set after a provider rejects concurrent requests. Successful maps stay
    /// accepted and all remaining segments run sequentially on resume.
    #[serde(default)]
    pub parallel_disabled: bool,
    /// Bounded number of times an overflowing map segment has been split more
    /// finely. Persisting it prevents restart loops against a bad window.
    #[serde(default)]
    pub repartitions: u8,
    #[serde(default)]
    pub corrections: u8,
}

#[derive(Clone, Debug)]
pub struct MapRequest {
    pub ordinal: usize,
    pub system: String,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub struct ReduceRequest {
    pub level: usize,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub final_group: bool,
    pub covered_ranges: Vec<ContextItemRange>,
    pub system: String,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub struct ValidationRequest {
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub system: String,
    pub prompt: String,
}

impl ContextManager {
    #[must_use]
    pub fn map_reduce_active(&self) -> bool {
        self.map_reduce.is_some()
    }

    #[must_use]
    pub fn compaction_staging_active(&self) -> bool {
        self.map_reduce_active() || self.split_active()
    }

    /// Freeze the currently planned checkpoint source into range-only map
    /// segments. Re-entering while active preserves all accepted work.
    pub fn begin_map_reduce(&mut self, window: usize) -> bool {
        if self.map_reduce.is_some() && !self.map_reduce_source_valid() {
            self.map_reduce = None;
        }
        if let Some(state) = self.map_reduce.as_mut() {
            // A larger model can resume accepted work. Smaller windows keep
            // their original segment references and repartition on overflow.
            state.window = state.window.min(window);
            return true;
        }
        let source = self.checkpoint_source_indices_for(window);
        if source.is_empty() {
            return false;
        }
        let source_item_ids = source.iter().map(|&idx| self.items[idx].id()).collect();
        let source_fingerprints = source
            .iter()
            .map(|&idx| {
                let item = &self.items[idx];
                (
                    item.id(),
                    xxhash_rust::xxh64::xxh64(serialize_item(item).as_bytes(), 0),
                )
            })
            .collect();
        let segments = self.partition_map_segments(&source, window);
        if segments.is_empty() {
            return false;
        }
        self.map_reduce = Some(MapReduceState {
            version: MAP_REDUCE_VERSION,
            window,
            model: None,
            source_item_ids,
            source_fingerprints,
            segments,
            phase: MapReducePhase::Mapping,
            level: 0,
            nodes: Vec::new(),
            next_nodes: Vec::new(),
            reduce_cursor: 0,
            candidate: None,
            validation_cursor: 0,
            audits: Vec::new(),
            final_summary: None,
            parallel_disabled: false,
            repartitions: 0,
            corrections: 0,
        });
        true
    }

    fn map_reduce_source_valid(&self) -> bool {
        self.map_reduce.as_ref().is_some_and(|state| {
            state.version == MAP_REDUCE_VERSION
                && state.source_item_ids.iter().all(|id| {
                    self.items
                        .iter()
                        .any(|item| item.id() == *id && !self.is_hidden(item))
                })
                && state.source_fingerprints.iter().all(|(id, hash)| {
                    self.items.iter().any(|item| {
                        item.id() == *id
                            && xxhash_rust::xxh64::xxh64(serialize_item(item).as_bytes(), 0)
                                == *hash
                    })
                })
        })
    }

    pub fn prepare_map_reduce_model(&mut self, model: &str, window: usize) {
        if let Some(state) = self.map_reduce.as_mut() {
            if state
                .model
                .as_deref()
                .is_some_and(|previous| previous != model)
            {
                state.window = window;
                state.repartitions = 0;
                state.parallel_disabled = false;
                state.corrections = 0;
                if state.level >= MAX_REDUCTION_LEVELS {
                    state.level = 0;
                }
            }
            state.model = Some(model.to_string());
        }
    }

    #[must_use]
    pub fn map_reduce_window(&self) -> usize {
        self.map_reduce
            .as_ref()
            .map_or(self.max_tokens, |state| state.window)
    }

    #[must_use]
    pub fn parallel_mapping_enabled(&self) -> bool {
        self.map_reduce
            .as_ref()
            .is_some_and(|state| state.phase == MapReducePhase::Mapping && !state.parallel_disabled)
    }

    pub fn disable_parallel_mapping(&mut self) {
        if let Some(state) = self.map_reduce.as_mut() {
            state.parallel_disabled = true;
        }
    }

    #[must_use]
    pub fn map_progress(&self) -> (usize, usize) {
        self.map_reduce.as_ref().map_or((0, 0), |state| {
            (
                state
                    .segments
                    .iter()
                    .filter(|segment| segment.summary.is_some())
                    .count(),
                state.segments.len(),
            )
        })
    }

    /// Split every incomplete map segment against a smaller observed window.
    /// Completed summaries remain attached to their original ordered ranges.
    /// Returns false when the bounded repartition budget is exhausted or the
    /// new partition cannot make any segment finer.
    pub fn repartition_incomplete_maps(&mut self, requested_window: Option<usize>) -> bool {
        let Some(mut state) = self.map_reduce.take() else {
            return false;
        };
        if state.phase != MapReducePhase::Mapping || state.repartitions >= MAX_REPARTITIONS {
            self.map_reduce = Some(state);
            return false;
        }
        let next_window = requested_window
            .filter(|window| *window < state.window)
            .unwrap_or_else(|| state.window.saturating_mul(3) / 4)
            .max(2);
        let budget = call_payload_budget(next_window, MAP_CALL_OVERHEAD);
        let old_slice_count: usize = state
            .segments
            .iter()
            .filter(|segment| segment.summary.is_none())
            .map(|segment| segment.slices.len())
            .sum();
        let old_segment_count = state
            .segments
            .iter()
            .filter(|segment| segment.summary.is_none())
            .count();
        let original_segments = state.segments.clone();
        let mut rebuilt = Vec::new();
        for segment in std::mem::take(&mut state.segments) {
            if segment.summary.is_some() {
                rebuilt.push(segment);
                continue;
            }
            let slices = self.split_slices_to_budget(&segment.slices, budget);
            let mut current = Vec::new();
            let mut held = 0usize;
            for slice in slices {
                let tokens = self.slice_tokens(&slice);
                if !current.is_empty() && held.saturating_add(tokens) > budget {
                    rebuilt.push(new_segment(0, std::mem::take(&mut current)));
                    held = 0;
                }
                held = held.saturating_add(tokens);
                current.push(slice);
            }
            if !current.is_empty() {
                rebuilt.push(new_segment(0, current));
            }
        }
        let new_slice_count: usize = rebuilt
            .iter()
            .filter(|segment| segment.summary.is_none())
            .map(|segment| segment.slices.len())
            .sum();
        let new_segment_count = rebuilt
            .iter()
            .filter(|segment| segment.summary.is_none())
            .count();
        if next_window >= state.window
            || (new_slice_count <= old_slice_count && new_segment_count <= old_segment_count)
        {
            state.segments = original_segments;
            self.map_reduce = Some(state);
            return false;
        }
        let next_ordinal = original_segments
            .iter()
            .map(|segment| segment.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        for (offset, segment) in rebuilt
            .iter_mut()
            .filter(|segment| segment.summary.is_none())
            .enumerate()
        {
            segment.ordinal = next_ordinal + offset;
        }
        state.window = next_window;
        state.segments = rebuilt;
        state.repartitions += 1;
        self.map_reduce = Some(state);
        true
    }

    #[must_use]
    pub fn pending_map_requests(&self) -> Vec<MapRequest> {
        let Some(state) = self.map_reduce.as_ref() else {
            return Vec::new();
        };
        if state.phase != MapReducePhase::Mapping {
            return Vec::new();
        }
        state
            .segments
            .iter()
            .filter(|segment| segment.summary.is_none())
            .filter_map(|segment| self.map_request(segment, state.window))
            .collect()
    }

    pub fn accept_map_summary(&mut self, ordinal: usize, summary: &str) -> bool {
        let summary = summary.trim();
        let Some(state) = self.map_reduce.as_mut() else {
            return false;
        };
        if state.phase != MapReducePhase::Mapping || summary.is_empty() {
            return false;
        }
        let Some(segment) = state
            .segments
            .iter_mut()
            .find(|segment| segment.ordinal == ordinal)
        else {
            return false;
        };
        if segment.summary.is_none() {
            segment.summary = Some(summary.to_string());
        }
        if state
            .segments
            .iter()
            .all(|segment| segment.summary.is_some())
        {
            state.nodes = state
                .segments
                .iter()
                .map(|segment| SummaryNode {
                    covered_ranges: segment.covered_ranges.clone(),
                    summary: segment.summary.clone().unwrap_or_default(),
                })
                .collect();
            state.phase = MapReducePhase::Reducing;
        }
        true
    }

    #[must_use]
    pub fn next_reduce_request(&self) -> Option<ReduceRequest> {
        let state = self.map_reduce.as_ref()?;
        if state.phase != MapReducePhase::Reducing
            || state.nodes.is_empty()
            || state.level >= MAX_REDUCTION_LEVELS
        {
            return None;
        }
        let start = state.reduce_cursor;
        if start >= state.nodes.len() {
            return None;
        }
        let budget = call_payload_budget(state.window, REDUCE_CALL_OVERHEAD);
        let mut end = start;
        let mut held = 0usize;
        for (idx, node) in state.nodes.iter().enumerate().skip(start) {
            let rendered = render_node(node);
            let tokens = self.encoding.estimate(&rendered);
            if end > start && held.saturating_add(tokens) > budget {
                break;
            }
            held = held.saturating_add(tokens);
            end = idx + 1;
        }
        let nodes = &state.nodes[start..end];
        let covered_ranges = merge_node_ranges(nodes);
        let summaries = nodes
            .iter()
            .map(render_node)
            .collect::<Vec<_>>()
            .join("\n\n");
        let final_group = start == 0 && end == state.nodes.len();
        Some(ReduceRequest {
            level: state.level,
            start,
            end,
            total: state.nodes.len(),
            final_group,
            covered_ranges: covered_ranges.clone(),
            system: REDUCE_SUMMARIZER_SYSTEM.to_string(),
            prompt: build_reduce_prompt(state.level, &covered_ranges, &summaries, final_group),
        })
    }

    pub fn accept_reduce_summary(&mut self, request: &ReduceRequest, summary: &str) -> bool {
        let summary = summary.trim();
        let Some(state) = self.map_reduce.as_mut() else {
            return false;
        };
        if state.phase != MapReducePhase::Reducing
            || state.level != request.level
            || state.reduce_cursor != request.start
            || summary.is_empty()
        {
            return false;
        }
        if request.final_group {
            state.candidate = Some(summary.to_string());
            state.phase = MapReducePhase::Validating;
            state.validation_cursor = 0;
            return true;
        }
        state.next_nodes.push(SummaryNode {
            covered_ranges: request.covered_ranges.clone(),
            summary: summary.to_string(),
        });
        state.reduce_cursor = request.end;
        if state.reduce_cursor >= state.nodes.len() {
            state.nodes = std::mem::take(&mut state.next_nodes);
            state.reduce_cursor = 0;
            state.level = state.level.saturating_add(1);
        }
        true
    }

    /// Parse a reducer's explicit conflict request. Only source-covered ranges
    /// are accepted and the request count is bounded.
    #[must_use]
    pub fn parse_reread_request(&self, response: &str) -> Vec<ContextItemRange> {
        let Some(spec) = response.trim().strip_prefix("REREAD:") else {
            return Vec::new();
        };
        let Some(state) = self.map_reduce.as_ref() else {
            return Vec::new();
        };
        let mut source = coalesce_ranges(&state.source_item_ids);
        for item in &self.items {
            if state.source_item_ids.contains(&item.id())
                && let super::ContextItem::Compaction { covered_ranges, .. } = item
            {
                source.extend(covered_ranges.iter().copied());
            }
        }
        // Follow checkpoint references only as far as the current branch's
        // immutable raw items are available. A removed item must not be
        // silently represented by its neighbor in a requested range.
        let available: Vec<u64> = self
            .items
            .iter()
            .filter_map(|item| {
                source
                    .iter()
                    .any(|range| range.start_id <= item.id() && item.id() <= range.end_id)
                    .then_some(item.id())
            })
            .collect();
        let source = coalesce_ranges(&available);
        let Some(ranges) = spec.split(',').map(parse_range).collect::<Option<Vec<_>>>() else {
            return Vec::new();
        };
        if ranges.len() > MAX_REREAD_RANGES
            || !ranges.iter().all(|requested| {
                source.iter().any(|allowed| {
                    allowed.start_id <= requested.start_id && requested.end_id <= allowed.end_id
                })
            })
        {
            return Vec::new();
        }
        ranges
    }

    #[must_use]
    pub fn conflict_reduce_request(
        &self,
        request: &ReduceRequest,
        ranges: &[ContextItemRange],
    ) -> Option<LlmCompactionRequest> {
        if ranges.is_empty() {
            return None;
        }
        let state = self.map_reduce.as_ref()?;
        let excerpts = ranges
            .iter()
            .map(|range| self.render_raw_range(*range, state.window))
            .collect::<Option<Vec<_>>>()?
            .join("\n\n");
        Some(LlmCompactionRequest {
            system: request.system.clone(),
            prompt: format!(
                "{}\n\nThe requested immutable source ranges follow. Resolve the conflict and return the final structured reduction; do not request another reread.\n\n{}",
                request.prompt, excerpts
            ),
        })
    }

    #[must_use]
    pub fn next_validation_request(&self) -> Option<ValidationRequest> {
        let state = self.map_reduce.as_ref()?;
        if state.phase != MapReducePhase::Validating {
            return None;
        }
        let candidate = state.candidate.as_deref()?;
        let start = state.validation_cursor;
        if start >= state.segments.len() {
            return None;
        }
        let budget = call_payload_budget(state.window, REDUCE_CALL_OVERHEAD)
            .saturating_sub(self.encoding.estimate(candidate));
        let mut end = start;
        let mut held = 0usize;
        for (idx, segment) in state.segments.iter().enumerate().skip(start) {
            let summary = segment.summary.as_deref().unwrap_or_default();
            let tokens = self.encoding.estimate(summary);
            if end > start && held.saturating_add(tokens) > budget {
                break;
            }
            held = held.saturating_add(tokens);
            end = idx + 1;
        }
        let summaries = state.segments[start..end]
            .iter()
            .map(|segment| {
                format!(
                    "[Map segment {} covering {}]\n{}",
                    segment.ordinal,
                    format_ranges(&segment.covered_ranges),
                    segment.summary.as_deref().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(ValidationRequest {
            start,
            end,
            total: state.segments.len(),
            system: VALIDATOR_SYSTEM.to_string(),
            prompt: build_validation_prompt(candidate, &summaries),
        })
    }

    pub fn accept_validation(&mut self, request: &ValidationRequest, audit: &str) -> bool {
        let audit = audit.trim();
        let Some(state) = self.map_reduce.as_mut() else {
            return false;
        };
        if state.phase != MapReducePhase::Validating
            || state.validation_cursor != request.start
            || audit.is_empty()
        {
            return false;
        }
        if !audit.eq_ignore_ascii_case("PASS") {
            state.audits.push(audit.to_string());
        }
        state.validation_cursor = request.end;
        if state.validation_cursor >= state.segments.len() {
            if state.audits.is_empty() {
                state.final_summary = state.candidate.clone();
                state.phase = MapReducePhase::Ready;
            } else {
                state.phase = MapReducePhase::Correcting;
            }
        }
        true
    }

    #[must_use]
    pub fn correction_request(&self) -> Option<LlmCompactionRequest> {
        let state = self.map_reduce.as_ref()?;
        if state.phase != MapReducePhase::Correcting || state.corrections >= MAX_CORRECTIONS {
            return None;
        }
        Some(LlmCompactionRequest {
            system: REDUCE_SUMMARIZER_SYSTEM.to_string(),
            prompt: build_correction_prompt(state.candidate.as_deref()?, &state.audits),
        })
    }

    pub fn accept_correction(&mut self, summary: &str) -> bool {
        let summary = summary.trim();
        let Some(state) = self.map_reduce.as_mut() else {
            return false;
        };
        if state.phase != MapReducePhase::Correcting
            || summary.is_empty()
            || summary.starts_with("REREAD:")
            || state.corrections >= MAX_CORRECTIONS
        {
            return false;
        }
        state.candidate = Some(summary.to_string());
        state.final_summary = None;
        state.validation_cursor = 0;
        state.audits.clear();
        state.corrections += 1;
        state.phase = MapReducePhase::Validating;
        true
    }

    pub fn commit_map_reduce(&mut self) -> bool {
        if !self.map_reduce_source_valid() {
            return false;
        }
        let Some(state) = self.map_reduce.take() else {
            return false;
        };
        if state.phase != MapReducePhase::Ready {
            self.map_reduce = Some(state);
            return false;
        }
        let Some(summary) = state.final_summary.clone() else {
            self.map_reduce = Some(state);
            return false;
        };
        let source: Vec<usize> = state
            .source_item_ids
            .iter()
            .filter_map(|id| self.items.iter().position(|item| item.id() == *id))
            .collect();
        if source.len() != state.source_item_ids.len()
            || !self.apply_summary_to_source_with_trigger(
                summary,
                &source,
                (state.window.saturating_mul(COMPACT_PCT) / 100).min(self.normal_trigger()),
            )
        {
            self.map_reduce = Some(state);
            return false;
        }
        true
    }

    pub fn abort_map_reduce(&mut self) {
        self.map_reduce = None;
    }

    fn partition_map_segments(&self, source: &[usize], window: usize) -> Vec<MapSegment> {
        let budget = call_payload_budget(window, MAP_CALL_OVERHEAD);
        let mut slices = Vec::new();
        for &idx in source {
            let item = &self.items[idx];
            let rendered = serialize_item(item);
            if rendered.is_empty() {
                continue;
            }
            let mut start = 0usize;
            while start < rendered.len() {
                let end = prefix_end_for_budget(&rendered, start, budget, self.encoding);
                slices.push(SegmentSlice {
                    item_id: item.id(),
                    start_byte: start,
                    end_byte: end,
                });
                start = end;
            }
        }

        let mut segments: Vec<MapSegment> = Vec::new();
        let mut current: Vec<SegmentSlice> = Vec::new();
        let mut held = 0usize;
        for slice in slices {
            let tokens = self.slice_tokens(&slice);
            if !current.is_empty() && held.saturating_add(tokens) > budget {
                segments.push(new_segment(segments.len(), std::mem::take(&mut current)));
                held = 0;
            }
            held = held.saturating_add(tokens);
            current.push(slice);
        }
        if !current.is_empty() {
            segments.push(new_segment(segments.len(), current));
        }
        segments
    }

    fn split_slices_to_budget(&self, source: &[SegmentSlice], budget: usize) -> Vec<SegmentSlice> {
        let mut slices = Vec::new();
        for slice in source {
            let Some(item) = self.items.iter().find(|item| item.id() == slice.item_id) else {
                continue;
            };
            let rendered = serialize_item(item);
            let Some(prefix) = rendered.get(..slice.end_byte) else {
                continue;
            };
            let mut start = slice.start_byte;
            while start < slice.end_byte {
                let end = prefix_end_for_budget(prefix, start, budget, self.encoding);
                slices.push(SegmentSlice {
                    item_id: slice.item_id,
                    start_byte: start,
                    end_byte: end,
                });
                start = end;
            }
        }
        slices
    }

    fn map_request(&self, segment: &MapSegment, window: usize) -> Option<MapRequest> {
        let text = self.render_segment(segment)?;
        let output_target = (window / 10).clamp(128, 2_000);
        Some(MapRequest {
            ordinal: segment.ordinal,
            system: MAP_SUMMARIZER_SYSTEM.to_string(),
            prompt: build_map_prompt(
                segment.ordinal,
                &segment.covered_ranges,
                &text,
                output_target,
            ),
        })
    }

    fn render_segment(&self, segment: &MapSegment) -> Option<String> {
        let mut out = String::new();
        for slice in &segment.slices {
            let item = self.items.iter().find(|item| item.id() == slice.item_id)?;
            let rendered = serialize_item(item);
            if slice.end_byte > rendered.len()
                || slice.start_byte >= slice.end_byte
                || !rendered.is_char_boundary(slice.start_byte)
                || !rendered.is_char_boundary(slice.end_byte)
            {
                return None;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!(
                "[Context item #{} bytes {}..{}]\n{}",
                slice.item_id,
                slice.start_byte,
                slice.end_byte,
                &rendered[slice.start_byte..slice.end_byte]
            ));
        }
        Some(out)
    }

    fn render_raw_range(&self, range: ContextItemRange, window: usize) -> Option<String> {
        let text = self
            .items
            .iter()
            .filter(|item| range.start_id <= item.id() && item.id() <= range.end_id)
            .map(serialize_item)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            return None;
        }
        let budget = call_payload_budget(window, REDUCE_CALL_OVERHEAD) / MAX_REREAD_RANGES;
        if self.encoding.estimate(&text) > budget {
            // A partial excerpt cannot establish which side of a conflict is
            // correct. Reject the reread instead of presenting truncated text
            // as if it covered the complete requested source range.
            return None;
        }
        Some(format!(
            "[Immutable source range #{}-#{}]\n{}",
            range.start_id, range.end_id, text
        ))
    }

    fn slice_tokens(&self, slice: &SegmentSlice) -> usize {
        self.items
            .iter()
            .find(|item| item.id() == slice.item_id)
            .map(serialize_item)
            .and_then(|rendered| {
                rendered
                    .get(slice.start_byte..slice.end_byte)
                    .map(str::to_owned)
            })
            .map_or(0, |text| self.encoding.estimate(&text))
    }
}

fn call_payload_budget(window: usize, overhead: usize) -> usize {
    let reserved = overhead.min(window / 2).max(1);
    window.saturating_sub(reserved).max(1)
}

fn prefix_end_for_budget(
    text: &str,
    start: usize,
    budget: usize,
    encoding: crate::util::TokenEncoding,
) -> usize {
    if encoding.estimate(&text[start..]) <= budget {
        return text.len();
    }
    let mut low = start + 1;
    let mut high = text.len();
    let mut best = start;
    while low <= high {
        let mid = text.floor_char_boundary(low + (high - low) / 2);
        if mid <= start {
            low = low.saturating_add(1);
            continue;
        }
        if encoding.estimate(&text[start..mid]) <= budget {
            best = mid;
            low = mid.saturating_add(1);
        } else {
            high = mid.saturating_sub(1);
        }
    }
    if best > start {
        best
    } else {
        text.ceil_char_boundary((start + 1).min(text.len()))
    }
}

fn new_segment(ordinal: usize, slices: Vec<SegmentSlice>) -> MapSegment {
    let mut ids: Vec<u64> = slices.iter().map(|slice| slice.item_id).collect();
    ids.sort_unstable();
    ids.dedup();
    MapSegment {
        ordinal,
        covered_ranges: coalesce_ranges(&ids),
        slices,
        summary: None,
    }
}

fn render_node(node: &SummaryNode) -> String {
    format!(
        "[Covered ranges: {}]\n{}",
        format_ranges(&node.covered_ranges),
        node.summary
    )
}

pub(super) fn format_ranges(ranges: &[ContextItemRange]) -> String {
    ranges
        .iter()
        .map(|range| format!("#{}-#{}", range.start_id, range.end_id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn merge_node_ranges(nodes: &[SummaryNode]) -> Vec<ContextItemRange> {
    merge_ranges(
        nodes
            .iter()
            .flat_map(|node| node.covered_ranges.iter().copied())
            .collect(),
    )
}

fn parse_range(value: &str) -> Option<ContextItemRange> {
    let value = value.trim().trim_start_matches('#');
    let (start, end) = value.split_once('-')?;
    let start_id = start.trim().trim_start_matches('#').parse().ok()?;
    let end_id = end.trim().trim_start_matches('#').parse().ok()?;
    (start_id <= end_id).then_some(ContextItemRange { start_id, end_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::context::ContextItem;

    fn large_manager(window: usize) -> ContextManager {
        let mut manager = ContextManager::new(window);
        manager.add_user(&"old immutable context áβ ".repeat(4_000));
        manager.add_assistant("recent raw tail", true);
        manager
    }

    #[test]
    fn map_segments_are_independent_ordered_and_exactly_slice_oversized_items() {
        let mut manager = large_manager(1_000);
        assert!(manager.begin_map_reduce(1_000));
        let state = manager.map_reduce.as_ref().unwrap();
        assert_eq!(state.version, MAP_REDUCE_VERSION);
        assert!(
            state.segments.len() > 1,
            "the oversized item is byte-sliced"
        );
        assert!(
            state
                .segments
                .windows(2)
                .all(|pair| pair[0].ordinal + 1 == pair[1].ordinal)
        );

        let mut by_item = std::collections::HashMap::<u64, Vec<SegmentSlice>>::new();
        for slice in state
            .segments
            .iter()
            .flat_map(|segment| segment.slices.iter().cloned())
        {
            by_item.entry(slice.item_id).or_default().push(slice);
        }
        for slices in by_item.values_mut() {
            slices.sort_unstable_by_key(|slice| slice.start_byte);
            assert_eq!(slices[0].start_byte, 0);
            assert!(
                slices
                    .windows(2)
                    .all(|pair| pair[0].end_byte == pair[1].start_byte)
            );
            let item = manager
                .items
                .iter()
                .find(|item| item.id() == slices[0].item_id)
                .unwrap();
            assert_eq!(slices.last().unwrap().end_byte, serialize_item(item).len());
        }

        let requests = manager.pending_map_requests();
        assert_eq!(requests.len(), state.segments.len());
        assert!(requests.iter().all(|request| {
            request.prompt.contains("## Source Ranges")
                && request.prompt.contains("## Open Work")
                && !request.prompt.contains("Continuation context")
        }));
    }

    #[test]
    fn accepted_map_results_are_idempotent_and_resume_without_regeneration() {
        let mut manager = large_manager(2_000);
        assert!(manager.begin_map_reduce(2_000));
        let first = manager.pending_map_requests()[0].ordinal;
        assert!(manager.accept_map_summary(first, "first durable map summary"));
        assert!(manager.accept_map_summary(first, "must not overwrite"));

        let saved = manager.save_state();
        let mut restored = ContextManager::new(2_000);
        restored.restore_state(&saved);
        let state = restored.map_reduce.as_ref().unwrap();
        assert_eq!(
            state.segments[first].summary.as_deref(),
            Some("first durable map summary")
        );
        assert!(
            restored
                .pending_map_requests()
                .iter()
                .all(|request| request.ordinal != first)
        );
    }

    #[test]
    fn reduction_recurses_over_every_ordered_map_summary() {
        let mut manager = large_manager(1_000);
        assert!(manager.begin_map_reduce(1_000));
        let ordinals: Vec<usize> = manager
            .pending_map_requests()
            .iter()
            .map(|request| request.ordinal)
            .collect();
        assert!(ordinals.len() > 2);
        for ordinal in ordinals {
            assert!(manager.accept_map_summary(
                ordinal,
                &format!("segment {ordinal} {}", "detail ".repeat(250))
            ));
        }

        let first = manager.next_reduce_request().unwrap();
        assert!(!first.final_group, "large maps require bounded grouping");
        assert!(first.end > first.start);
        assert!(manager.accept_reduce_summary(&first, "intermediate group"));

        let mut calls = 1usize;
        loop {
            let request = manager.next_reduce_request().unwrap();
            calls += 1;
            let final_group = request.final_group;
            let summary = if final_group {
                "## Objective\n- reconciled final checkpoint"
            } else {
                "intermediate group"
            };
            assert!(manager.accept_reduce_summary(&request, summary));
            if final_group {
                break;
            }
            assert!(calls < 100, "hierarchical reduction must make progress");
        }
        assert!(calls > 2);
        assert_eq!(
            manager.map_reduce.as_ref().unwrap().phase,
            MapReducePhase::Validating
        );
    }

    #[test]
    fn reducer_can_request_only_bounded_frozen_raw_ranges() {
        let mut manager = large_manager(2_000);
        manager.add_user("small immutable evidence");
        manager.add_assistant("acknowledged", true);
        manager.begin_manual_compaction();
        assert!(manager.begin_map_reduce(2_000));
        let ordinals: Vec<usize> = manager
            .pending_map_requests()
            .iter()
            .map(|request| request.ordinal)
            .collect();
        for ordinal in ordinals {
            assert!(manager.accept_map_summary(ordinal, "map fact"));
        }
        let request = manager.next_reduce_request().unwrap();
        let source_id = 3;
        let parsed = manager.parse_reread_request(&format!(
            "REREAD: {source_id}-{source_id}, 99999-100000, {source_id}-{source_id}, {source_id}-{source_id}"
        ));
        assert!(
            parsed.is_empty(),
            "an invalid control reply is rejected atomically"
        );
        let parsed = manager.parse_reread_request("REREAD: 3-3");
        let conflict = manager
            .conflict_reduce_request(&request, &parsed[..1])
            .unwrap();
        assert!(conflict.prompt.contains("Immutable source range"));
        assert!(conflict.prompt.contains("small immutable evidence"));
        assert!(conflict.prompt.contains("do not request another reread"));
        assert!(
            manager
                .conflict_reduce_request(
                    &request,
                    &[ContextItemRange {
                        start_id: 1,
                        end_id: 1
                    }]
                )
                .is_none(),
            "an oversized raw range cannot silently be truncated"
        );
    }

    #[test]
    fn reducer_can_reread_raw_evidence_behind_a_previous_checkpoint() {
        let mut manager = ContextManager::new(8_000);
        manager.add_user("Never publish secrets");
        manager.add_assistant("Verified old state", true);
        manager.begin_manual_compaction();
        assert!(manager.apply_llm_summary("Prior checkpoint cites #1".into()));
        manager.add_user("Continue with the updated file");
        manager.add_assistant("File changed; old observation is stale", true);
        assert!(manager.begin_map_reduce(8_000));
        for request in manager.pending_map_requests() {
            assert!(manager.accept_map_summary(request.ordinal, "Conflicting facts"));
        }
        let request = manager.next_reduce_request().unwrap();
        let ranges = manager.parse_reread_request("REREAD: 1-1");
        assert_eq!(ranges.len(), 1);
        let reread = manager.conflict_reduce_request(&request, &ranges).unwrap();
        assert!(reread.prompt.contains("Never publish secrets"));
        manager.items.retain(|item| item.id() != 1);
        assert!(manager.parse_reread_request("REREAD: 1-2").is_empty());
    }

    #[test]
    fn validation_correction_gates_an_atomic_checkpoint_commit() {
        let mut manager = large_manager(2_000);
        let before = manager.items.clone();
        assert!(manager.begin_map_reduce(2_000));
        let ordinals: Vec<usize> = manager
            .pending_map_requests()
            .iter()
            .map(|request| request.ordinal)
            .collect();
        for ordinal in ordinals {
            assert!(manager.accept_map_summary(
                ordinal,
                "## Objective and Constraints\n- preserve the user constraint\n## Open Work\n- finish the refactor"
            ));
        }
        let reduction = manager.next_reduce_request().unwrap();
        assert!(reduction.final_group);
        assert!(manager.accept_reduce_summary(&reduction, "## Objective\n- incomplete candidate"));
        assert!(
            !manager.commit_map_reduce(),
            "unvalidated state cannot commit"
        );
        assert_eq!(manager.items.len(), before.len());

        while let Some(validation) = manager.next_validation_request() {
            assert!(manager.accept_validation(
                &validation,
                "Restore the user constraint and open refactor work."
            ));
        }
        assert_eq!(
            manager.map_reduce.as_ref().unwrap().phase,
            MapReducePhase::Correcting
        );
        let correction = manager.correction_request().unwrap();
        assert!(correction.prompt.contains("Restore the user constraint"));
        assert!(manager.accept_correction(
            "## Objective\n- preserve the user constraint\n\n## Work State\n### Active\n- finish the refactor"
        ));
        assert!(
            !manager.commit_map_reduce(),
            "corrections must be audited again"
        );
        while let Some(validation) = manager.next_validation_request() {
            assert!(manager.accept_validation(&validation, "PASS"));
        }
        assert!(manager.commit_map_reduce());
        assert!(!manager.map_reduce_active());
        let ContextItem::Compaction {
            summary,
            covered_ranges,
            ..
        } = manager.items.back().unwrap()
        else {
            panic!("validated checkpoint must be appended");
        };
        assert!(summary.contains("finish the refactor"));
        assert!(!covered_ranges.is_empty());
        assert!(
            manager.items.len() > before.len(),
            "raw history remains intact"
        );
    }

    #[test]
    fn oversized_validated_checkpoint_is_rejected_without_changing_the_view() {
        let mut manager = large_manager(1_000);
        let visible_before = manager.build_messages("");
        assert!(manager.begin_map_reduce(1_000));
        let source_ids = manager.map_reduce.as_ref().unwrap().source_item_ids.clone();
        let ordinals: Vec<usize> = manager
            .pending_map_requests()
            .iter()
            .map(|request| request.ordinal)
            .collect();
        for ordinal in ordinals {
            assert!(manager.accept_map_summary(ordinal, "small map"));
        }
        let reduction = manager.next_reduce_request().unwrap();
        assert!(manager.accept_reduce_summary(&reduction, &"oversized ".repeat(2_000)));
        while let Some(validation) = manager.next_validation_request() {
            assert!(manager.accept_validation(&validation, "PASS"));
        }

        assert!(!manager.commit_map_reduce());
        assert_eq!(manager.build_messages("").len(), visible_before.len());
        assert_eq!(
            manager.map_reduce.as_ref().unwrap().source_item_ids,
            source_ids,
            "the ready staging remains resumable for a later recovery"
        );
        assert!(
            !manager
                .items
                .iter()
                .any(|item| matches!(item, ContextItem::Compaction { .. }))
        );
    }

    #[test]
    fn replaced_source_invalidates_staging_and_new_model_resets_recovery_limits() {
        let mut manager = large_manager(2_000);
        assert!(manager.begin_map_reduce(2_000));
        manager.prepare_map_reduce_model("small", 2_000);
        assert!(manager.accept_map_summary(0, "stale fact"));
        let mut state = manager.save_state();
        state.items[0] = ContextItem::User {
            id: 1,
            original: "changed source".repeat(2_000),
        };
        manager.restore_state(&state);
        assert!(!manager.map_reduce_source_valid());
        assert!(manager.begin_map_reduce(2_000));
        assert_eq!(manager.map_progress().0, 0);
        manager.prepare_map_reduce_model("small", 2_000);
        manager.disable_parallel_mapping();
        assert!(manager.repartition_incomplete_maps(Some(1_000)));
        manager.prepare_map_reduce_model("larger", 8_000);
        let state = manager.save_state().map_reduce.unwrap();
        assert_eq!(state.window, 8_000);
        assert_eq!(state.repartitions, 0);
        assert!(!state.parallel_disabled);
    }

    #[test]
    fn repeated_failed_corrections_never_commit_and_are_bounded() {
        let mut manager = large_manager(2_000);
        assert!(manager.begin_map_reduce(2_000));
        manager.prepare_map_reduce_model("small", 2_000);
        for request in manager.pending_map_requests() {
            assert!(manager.accept_map_summary(request.ordinal, "constraint: do not delete"));
        }
        let request = manager.next_reduce_request().unwrap();
        assert!(manager.accept_reduce_summary(&request, "incomplete candidate"));
        for _ in 0..MAX_CORRECTIONS {
            while let Some(request) = manager.next_validation_request() {
                assert!(manager.accept_validation(&request, "restore the constraint"));
            }
            assert!(manager.correction_request().is_some());
            assert!(manager.accept_correction("still incomplete"));
        }
        while let Some(request) = manager.next_validation_request() {
            assert!(manager.accept_validation(&request, "restore the constraint"));
        }
        assert!(manager.correction_request().is_none());
        assert!(!manager.commit_map_reduce());
        manager.prepare_map_reduce_model("small", 2_000);
        assert!(manager.correction_request().is_none());
        manager.prepare_map_reduce_model("larger", 8_000);
        assert!(manager.correction_request().is_some());
        assert!(manager.accept_correction("constraint: do not delete"));
        while let Some(request) = manager.next_validation_request() {
            assert!(manager.accept_validation(&request, "PASS"));
        }
        assert!(manager.commit_map_reduce());
    }

    #[test]
    fn reduction_limit_prevents_non_shrinking_model_output_loops() {
        let mut manager = large_manager(1_000);
        assert!(manager.begin_map_reduce(1_000));
        manager.prepare_map_reduce_model("small", 1_000);
        for request in manager.pending_map_requests() {
            assert!(manager.accept_map_summary(request.ordinal, &"no compression ".repeat(300)));
        }
        let mut calls = 0;
        while let Some(request) = manager.next_reduce_request() {
            assert!(manager.accept_reduce_summary(&request, &"no compression ".repeat(300)));
            calls += 1;
            assert!(calls < 1_000);
        }
        assert_eq!(
            manager.map_reduce.as_ref().unwrap().level,
            MAX_REDUCTION_LEVELS
        );
        assert!(!manager.commit_map_reduce());
        manager.prepare_map_reduce_model("small", 1_000);
        assert!(manager.next_reduce_request().is_none());
        manager.prepare_map_reduce_model("larger", 8_000);
        assert!(manager.next_reduce_request().is_some());
    }

    #[test]
    fn manual_checkpoint_leaves_unanswered_queued_inputs_verbatim() {
        let mut manager = ContextManager::new(8_000);
        manager.add_user("original task");
        manager.add_assistant("work completed so far", true);
        manager.add_user("queued constraint: preserve the API");
        manager.add_user("queued next action: run tests");
        manager.begin_manual_compaction();
        assert!(manager.begin_map_reduce(8_000));
        let state = manager.save_state().map_reduce.unwrap();
        assert_eq!(state.source_item_ids, vec![1, 2]);
        for request in manager.pending_map_requests() {
            assert!(!request.prompt.contains("queued constraint"));
            assert!(manager.accept_map_summary(request.ordinal, "completed work"));
        }
        let request = manager.next_reduce_request().unwrap();
        assert!(manager.accept_reduce_summary(&request, "checkpoint"));
        while let Some(request) = manager.next_validation_request() {
            assert!(manager.accept_validation(&request, "PASS"));
        }
        assert!(manager.commit_map_reduce());
        let messages = manager.build_messages("");
        assert!(messages.iter().any(
            |message| message.content.as_deref() == Some("queued constraint: preserve the API")
        ));
        assert!(
            messages
                .iter()
                .any(|message| message.content.as_deref() == Some("queued next action: run tests"))
        );
    }
}
