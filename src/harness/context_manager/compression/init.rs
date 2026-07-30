use text_splitter::TextSplitter;

use super::hierarchical::compress_hierarchical;
use super::lsa::compute_lsa;
use super::mmr::compress;
use super::tfidf::build_tfidf;

/// Compress text by splitting into sentence-like chunks (via text-splitter),
/// then running TF-IDF → LSA → MMR to select the most informative subset.
pub fn init(val: &str, use_hierarchical: bool, max_df: f64, lambda: f64, ratio: f64) -> String {
    // Split text at sentence boundaries using text-splitter's hierarchical
    // approach: paragraphs → sentences → words. A moderate character capacity
    // ensures we get sentence-level chunks for the TF-IDF pipeline.
    let splitter = TextSplitter::new(200);
    let sentences: Vec<String> = splitter.chunks(val).map(|s| s.to_string()).collect();

    let tfidf = build_tfidf(&sentences, max_df);
    let lsa = compute_lsa(&tfidf);

    let result = if use_hierarchical {
        compress_hierarchical(&sentences, &lsa.scores, &lsa.topic_vectors, lambda, ratio)
    } else {
        compress(&sentences, &lsa.scores, &lsa.topic_vectors, lambda, ratio)
    };

    result.selected.concat()
}
