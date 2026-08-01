use regex::Regex;
use std::sync::OnceLock;

use super::mmr::{MmrOutput, fallback_indices, mmr_select};

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(#{1,6})\s").unwrap())
}

/// Compresses sentences preserving Markdown headings while applying
/// full MMR selection (relevance + diversity) on the remaining content.
///
/// Approach:
///   1. Split sentences into **headings** and **content**.
///   2. Keep every heading unconditionally (structural context).
///   3. Run MMR on content only with the same λ and compression_ratio.
///   4. Merge the two lists sorted by original position.
///
/// This is strictly better than flat MMR: same compression rate and
/// diversity on content, but headings are never lost, so the LLM
/// always sees the section structure.
pub fn compress_hierarchical(
    sentences: &[String],
    scores: &[f64],
    topic_vectors: &[Vec<f64>],
    lambda: f64,
    compression_ratio: f64,
) -> MmrOutput {
    let tokens_before: usize = sentences.iter().map(|s| s.split_whitespace().count()).sum();

    // Separate heading indices from content indices
    let mut heading_indices: Vec<usize> = Vec::new();
    let mut content_indices: Vec<usize> = Vec::new();

    for (i, s) in sentences.iter().enumerate() {
        if heading_re().is_match(s) {
            heading_indices.push(i);
        } else {
            content_indices.push(i);
        }
    }

    // Run MMR on content only. If the scoring pipeline degenerated (e.g. every
    // bigram filtered by max_df), `scores` is empty and indexing it would panic:
    // fall back to a deterministic subset instead.
    let content_selected = if scores.is_empty() {
        fallback_indices(content_indices.len(), compression_ratio)
    } else {
        let content_scores: Vec<f64> = content_indices.iter().map(|&i| scores[i]).collect();
        let content_vectors: Vec<Vec<f64>> = content_indices
            .iter()
            .map(|&i| topic_vectors[i].clone())
            .collect();
        mmr_select(&content_scores, &content_vectors, lambda, compression_ratio)
    };

    // Map local MMR indices back to global sentence indices
    let mut selected: Vec<usize> = heading_indices;
    for &local_idx in &content_selected {
        selected.push(content_indices[local_idx]);
    }

    // Reorder by original position
    selected.sort_unstable();

    let selected_sentences: Vec<String> = selected.iter().map(|&i| sentences[i].clone()).collect();
    let tokens_after: usize = selected_sentences
        .iter()
        .map(|s| s.split_whitespace().count())
        .sum();

    MmrOutput {
        selected: selected_sentences,
        indices: selected,
        tokens_before,
        tokens_after,
    }
}
