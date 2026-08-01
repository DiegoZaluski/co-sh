use super::super::init;

#[test]
fn max_df_filter_does_not_crash() {
    let text = "the quick brown fox. the lazy dog. the sun is bright. ".repeat(5);
    let _ = init(&text, false, 0.5, 0.7, 0.4);
    // Smoke test: just verify it doesn't panic with max_df filtering
}

// Regression: highly repetitive text has every bigram filtered by max_df,
// which used to collapse the whole document to "" (flat) or panic
// (hierarchical). Non-empty input must never produce empty output.
#[test]
fn repetitive_text_never_returns_empty() {
    let text = "This is the first sentence. ".repeat(100);

    let flat = init(&text, false, 0.8, 0.7, 0.4);
    assert!(!flat.is_empty(), "flat compression dropped all content");
    assert!(
        flat.len() < text.len(),
        "repetitive content should still compress"
    );

    let hierarchical = init(&text, true, 0.8, 0.7, 0.4);
    assert!(
        !hierarchical.is_empty(),
        "hierarchical compression dropped all content"
    );
    assert!(
        hierarchical.len() < text.len(),
        "repetitive content should still compress"
    );
}
