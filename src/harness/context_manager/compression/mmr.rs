/// Result of the MMR selection + reordering.
pub struct MmrOutput {
    /// Selected sentences in original order
    pub selected: Vec<String>,
    /// Original indices of selected sentences (in reading order)
    pub indices: Vec<usize>,
    /// Token count before compression
    pub tokens_before: usize,
    /// Token count after compression
    pub tokens_after: usize,
}

// Token counting — approximate by whitespace splitting
fn count_tokens(text: &str) -> usize {
    text.split_whitespace().count()
}

// Cosine similarity between two topic vectors
fn cosine_sim(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    let denom = norm_a * norm_b;
    if denom < 1e-12 { 0.0 } else { dot / denom }
}

// Compute centroid of topic vectors
fn centroid(vectors: &[Vec<f64>]) -> Vec<f64> {
    let n = vectors.len();
    if n == 0 {
        return vec![];
    }
    let dim = vectors[0].len();
    let mut c = vec![0.0_f64; dim];
    for v in vectors {
        for (i, &val) in v.iter().enumerate() {
            c[i] += val;
        }
    }
    for val in &mut c {
        *val /= n as f64;
    }
    c
}

// MMR selection
//
// Selects sentences using Maximal Marginal Relevance.
// - lambda: relevance vs. diversity trade-off (0.7)
// - compression_ratio: fraction of sentences to keep (0.4)
//
// Returns indices of selected sentences in original reading order.

pub fn mmr_select(
    scores: &[f64],
    topic_vectors: &[Vec<f64>],
    lambda: f64,
    compression_ratio: f64,
) -> Vec<usize> {
    let n = scores.len();
    if n == 0 {
        return vec![];
    }

    let min_sentences = std::cmp::min(n, 4);
    let target = ((n as f64) * compression_ratio).ceil() as usize;
    let target = std::cmp::max(min_sentences, std::cmp::min(target, n));

    let centroid = centroid(topic_vectors);

    // Relevance score: mix of LSA score + centroid similarity
    // to produce a well-scaled Sim1
    let max_score = scores.iter().cloned().fold(0.0_f64, f64::max);
    let relevance: Vec<f64> = scores
        .iter()
        .map(|s| {
            if max_score > 1e-12 {
                s / max_score
            } else {
                0.0
            }
        })
        .collect();

    // MMR greedy selection loop
    let mut selected: Vec<usize> = Vec::with_capacity(target);
    let mut candidate_indices: Vec<usize> = (0..n).collect();

    for _ in 0..target {
        let mut best_score = -f64::INFINITY;
        let mut best_idx = 0;
        let mut best_pos = 0;

        for (pos, &i) in candidate_indices.iter().enumerate() {
            let sim1 = if !relevance.is_empty() {
                // Blend LSA score with centroid similarity
                let centroid_sim = cosine_sim(&topic_vectors[i], &centroid);
                0.5 * relevance[i] + 0.5 * centroid_sim
            } else {
                cosine_sim(&topic_vectors[i], &centroid)
            };

            let max_sim2 = selected
                .iter()
                .map(|&j| cosine_sim(&topic_vectors[i], &topic_vectors[j]))
                .fold(0.0_f64, f64::max);

            let mmr = lambda * sim1 - (1.0 - lambda) * max_sim2;

            if mmr > best_score {
                best_score = mmr;
                best_idx = i;
                best_pos = pos;
            }
        }

        selected.push(best_idx);
        candidate_indices.swap_remove(best_pos);
    }

    // Reorder by original position (Task 6)
    selected.sort_unstable();
    selected
}

// Orchestrator: compress sentences using MMR

/// Compresses a list of sentences using LSA + MMR.
/// - `lambda`: MMR relevance/diversity balance (default 0.7)
/// - `compression_ratio`: fraction of sentences to keep (default 0.4)
pub fn compress(
    sentences: &[String],
    scores: &[f64],
    topic_vectors: &[Vec<f64>],
    lambda: f64,
    compression_ratio: f64,
) -> MmrOutput {
    let tokens_before: usize = sentences.iter().map(|s| count_tokens(s)).sum();

    let indices = mmr_select(scores, topic_vectors, lambda, compression_ratio);

    let selected: Vec<String> = indices.iter().map(|&i| sentences[i].clone()).collect();
    let tokens_after: usize = selected.iter().map(|s| count_tokens(s)).sum();

    MmrOutput {
        selected,
        indices,
        tokens_before,
        tokens_after,
    }
}
