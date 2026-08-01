use text_splitter::TextSplitter;

use super::hierarchical::compress_hierarchical;
use super::lsa::compute_lsa;
use super::mmr::compress;
use super::tfidf::build_tfidf;

/// Character capacity for the sentence splitter. The text-splitter crate
/// splits at paragraph/sentence/word boundaries while staying within this
/// many characters per chunk.
pub(crate) const CHUNK_CAPACITY: usize = 200;

/// Compress text by splitting into sentence-like chunks (via text-splitter),
/// then running TF-IDF → LSA → MMR to select the most informative subset.
pub fn init(val: &str, use_hierarchical: bool, max_df: f64, lambda: f64, ratio: f64) -> String {
    // Split text at sentence boundaries using text-splitter's hierarchical
    // approach: paragraphs → sentences → words. A moderate character capacity
    // ensures we get sentence-level chunks for the TF-IDF pipeline.
    let splitter = TextSplitter::new(CHUNK_CAPACITY);
    let sentences: Vec<String> = splitter.chunks(val).map(|s| s.to_string()).collect();

    let tfidf = build_tfidf(&sentences, max_df);
    let lsa = compute_lsa(&tfidf);

    let result = if use_hierarchical {
        compress_hierarchical(&sentences, &lsa.scores, &lsa.topic_vectors, lambda, ratio)
    } else {
        compress(&sentences, &lsa.scores, &lsa.topic_vectors, lambda, ratio)
    };

    // text-splitter strips boundary whitespace, so rejoin with a single space
    // — concatenating directly glues the last word of one sentence to the
    // first word of the next.
    result.selected.join(" ")
}
