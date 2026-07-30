use nalgebra::{DMatrix, SVD};

use super::tfidf::TfIdfMatrix;

/// Result of LSA computation: scores per sentence and topic vectors.
pub struct LsaResult {
    /// Relevance score: L2 norm of each TF-IDF row (information density
    /// in the original bigram space, before SVD truncation).
    pub scores: Vec<f64>,
    /// Number of topics used
    pub n_topics: usize,
    /// Topic vectors (U_k * Sigma_k): rows = sentences, cols = topics.
    /// Used for diversity comparisons (cosine similarity in MMR).
    pub topic_vectors: Vec<Vec<f64>>,
}

// LSA computation — Truncated SVD + TF-IDF relevance scoring

/// Runs LSA: truncated SVD on the TF-IDF matrix.
///
/// `scores` come from the TF-IDF L2 norm (information density before
/// SVD truncation), not from the truncated topic space. This ensures
/// sentences with unique vocabulary (paths, code, errors) get
/// meaningful scores even when their signal lives in later dimensions.
///
/// `topic_vectors` come from U_k * Sigma_k and are used for diversity
/// comparisons in the MMR loop.
///
/// `n_topics` = `max(3, n_sentences / 3)` clamped to matrix rank.
pub fn compute_lsa(tfidf: &TfIdfMatrix) -> LsaResult {
    let n_sentences = tfidf.matrix.len();
    let n_terms = if n_sentences > 0 {
        tfidf.matrix[0].len()
    } else {
        0
    };

    if n_sentences == 0 || n_terms == 0 {
        return LsaResult {
            scores: vec![],
            n_topics: 0,
            topic_vectors: vec![],
        };
    }

    // Convert Vec<Vec<f64>> to DMatrix<f64> (row-major)
    let flat: Vec<f64> = tfidf
        .matrix
        .iter()
        .flat_map(|row| row.iter())
        .copied()
        .collect();
    let matrix = DMatrix::from_row_slice(n_sentences, n_terms, &flat);

    // Compute full SVD
    let svd = SVD::new(matrix, true, true);

    let u = svd.u.expect("U matrix should be computed");
    let singular_values = svd.singular_values;

    // Truncate to n_topics
    let max_rank = singular_values.len();
    let target = std::cmp::max(3, n_sentences / 3);
    let n_topics = std::cmp::min(target, max_rank);

    if n_topics == 0 {
        return LsaResult {
            scores: vec![0.0; n_sentences],
            n_topics: 0,
            topic_vectors: vec![],
        };
    }

    // Build U_k * Sigma_k  (n_sentences × n_topics)

    let u_k = u.columns(0, n_topics).into_owned();

    let mut topic_matrix = DMatrix::zeros(n_sentences, n_topics);
    for col in 0..n_topics {
        let s = singular_values[col];
        for row in 0..n_sentences {
            topic_matrix[(row, col)] = u_k[(row, col)] * s;
        }
    }

    // Build topic vectors from U_k * Sigma_k for MMR diversity

    let topic_vectors: Vec<Vec<f64>> = (0..n_sentences)
        .map(|row| (0..n_topics).map(|col| topic_matrix[(row, col)]).collect())
        .collect();

    // Score: L2 norm of each TF-IDF row (information density in
    // original bigram space, before SVD truncation).
    //
    // Why TF-IDF space instead of LSA topic space?
    // The SVD truncation discards later dimensions where unique
    // vocabulary (paths, error messages, code) lives. By scoring
    // in the original TF-IDF space, we preserve informativeness
    // for every sentence regardless of SVD rank.
    let mut scores: Vec<f64> = (0..n_sentences)
        .map(|row| {
            let sum_sq: f64 = (0..n_terms).map(|col| tfidf.matrix[row][col].powi(2)).sum();
            sum_sq.sqrt()
        })
        .collect();

    // Score floor — 2% of max ensures no sentence is completely
    // ignored, but low-information headers still get low scores.
    let max_score = scores.iter().cloned().fold(0.0_f64, f64::max);
    if max_score > 1e-12 {
        let floor = max_score * 0.02;
        for score in &mut scores {
            if *score < floor {
                *score = floor;
            }
        }
    }

    LsaResult {
        scores,
        n_topics,
        topic_vectors,
    }
}
