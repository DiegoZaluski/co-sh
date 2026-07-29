use super::super::init;

#[test]
fn short_text_preserves_content() {
    let result = init("Hello world. This is a test.", false, 0.8, 0.7, 0.4);
    assert!(!result.is_empty(), "short text should produce output");
    assert!(
        result.contains("Hello") || result.contains("test"),
        "output should retain text from the original input"
    );
}

#[test]
fn long_text_reduces_token_count() {
    let text = "This is the first sentence. ".repeat(100);
    let tokens_before = text.split_whitespace().count();
    let result = init(&text, false, 0.8, 0.7, 0.4);
    let tokens_after = result.split_whitespace().count();
    assert!(
        tokens_after <= tokens_before,
        "compression should not increase token count"
    );
    assert!(
        tokens_after < tokens_before / 2,
        "compression should reduce tokens significantly"
    );
}

#[test]
fn high_ratio_keeps_more_text() {
    let text = "Sentence one. Sentence two. Sentence three. Sentence four. Sentence five.";
    let low = init(text, false, 0.8, 0.7, 0.2);
    let high = init(text, false, 0.8, 0.7, 0.8);
    let low_tokens = low.split_whitespace().count();
    let high_tokens = high.split_whitespace().count();
    assert!(
        high_tokens >= low_tokens,
        "higher compression ratio should keep more text"
    );
}

#[test]
fn max_df_filter_does_not_crash() {
    let text = "the quick brown fox. the lazy dog. the sun is bright. ".repeat(5);
    let _ = init(&text, false, 0.5, 0.7, 0.4);
    // Smoke test: just verify it doesn't panic with max_df filtering
}

#[test]
fn lambda_zero_still_reduces_tokens() {
    let text = "Python is great. Python is fast. Python is fun. JavaScript is also good. Rust is safe.";
    let tokens_before = text.split_whitespace().count();
    let result = init(text, false, 0.8, 0.0, 0.5);
    assert!(!result.is_empty(), "lambda=0 should still produce output");
    let tokens_after = result.split_whitespace().count();
    assert!(
        tokens_after <= tokens_before,
        "lambda=0 compression should not increase token count"
    );
}

#[test]
fn lambda_one_still_reduces_tokens() {
    let text = "Python is great. Python is fast. Python is fun. JavaScript is also good. Rust is safe.";
    let tokens_before = text.split_whitespace().count();
    let result = init(text, false, 0.8, 1.0, 0.5);
    assert!(!result.is_empty(), "lambda=1 should still produce output");
    let tokens_after = result.split_whitespace().count();
    assert!(
        tokens_after <= tokens_before,
        "lambda=1 compression should not increase token count"
    );
}
