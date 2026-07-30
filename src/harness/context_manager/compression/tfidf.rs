use std::collections::HashMap;
use std::collections::HashSet;

/// Result of TF-IDF matrix construction.
pub struct TfIdfMatrix {
    /// Dense term-document matrix: rows = sentences, cols = bigrams
    pub matrix: Vec<Vec<f64>>,
    /// Bigram vocabulary (column labels)
    pub vocabulary: Vec<String>,
}

// Bigram extraction from a sentence

fn extract_bigrams(sentence: &str) -> Vec<String> {
    let tokens: Vec<&str> = sentence
        .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|t| !t.is_empty())
        .collect();

    tokens
        .windows(2)
        .map(|w| format!("{} {}", w[0], w[1]))
        .collect()
}

// Sublinear TF: 1 + log10(tf) when tf > 0

fn sublinear_tf(raw_tf: f64) -> f64 {
    if raw_tf > 0.0 {
        1.0 + raw_tf.log10()
    } else {
        0.0
    }
}

// TF-IDF matrix builder
//
// Builds a TF-IDF matrix from sentences using word bigrams.
// - sublinear_tf: applies 1 + log10(tf) normalization
// - max_df: ignores terms appearing in more than this fraction of docs

pub fn build_tfidf(sentences: &[String], max_df: f64) -> TfIdfMatrix {
    let num_docs = sentences.len();

    if num_docs == 0 {
        return TfIdfMatrix {
            matrix: Vec::new(),
            vocabulary: Vec::new(),
        };
    }

    // Extract bigrams and count document frequency

    let mut doc_freq: HashMap<String, usize> = HashMap::new();
    let mut all_bigrams: Vec<Vec<String>> = Vec::with_capacity(num_docs);

    for sentence in sentences {
        let bigrams = extract_bigrams(sentence);
        let unique: HashSet<&str> = bigrams.iter().map(|s| s.as_str()).collect();
        for bg in &unique {
            *doc_freq.entry(bg.to_string()).or_insert(0) += 1;
        }
        all_bigrams.push(bigrams);
    }

    // Filter by max_df and build vocabulary

    let max_docs = (max_df * num_docs as f64).ceil() as usize;
    let mut vocab: Vec<String> = doc_freq
        .into_iter()
        .filter(|(_, df)| *df <= max_docs)
        .map(|(term, _)| term)
        .collect();
    vocab.sort();

    if vocab.is_empty() {
        return TfIdfMatrix {
            matrix: Vec::new(),
            vocabulary: Vec::new(),
        };
    }

    // Recompute document frequencies for surviving terms

    let vocab_set: HashSet<&str> = vocab.iter().map(|s| s.as_str()).collect();
    let mut doc_freq_surviving: HashMap<&str, usize> = HashMap::new();
    for bigrams in &all_bigrams {
        let unique: HashSet<&str> = bigrams.iter().map(|s| s.as_str()).collect();
        for bg in unique {
            if vocab_set.contains(bg) {
                *doc_freq_surviving.entry(bg).or_insert(0) += 1;
            }
        }
    }

    let term_to_idx: HashMap<&str, usize> = vocab
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let n = num_docs as f64;

    // Fill matrix with TF-IDF values

    let mut matrix = vec![vec![0.0_f64; vocab.len()]; num_docs];

    for (doc_idx, bigrams) in all_bigrams.iter().enumerate() {
        let mut tf_counts: HashMap<usize, f64> = HashMap::new();
        for bg in bigrams {
            if let Some(&col) = term_to_idx.get(bg.as_str()) {
                *tf_counts.entry(col).or_insert(0.0) += 1.0;
            }
        }

        for (&col, &raw_count) in &tf_counts {
            let tf = sublinear_tf(raw_count);
            let term = &vocab[col];
            let df = doc_freq_surviving[term.as_str()] as f64;
            let idf = (n / df).log10();
            matrix[doc_idx][col] = tf * idf;
        }
    }

    TfIdfMatrix {
        matrix,
        vocabulary: vocab,
    }
}
