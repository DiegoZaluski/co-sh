use super::super::init;

#[test]
fn empty_text_returns_empty_string() {
    let result = init("", false, 0.8, 0.7, 0.4);
    assert!(result.is_empty(), "empty input should produce empty output");
}

#[test]
fn whitespace_only_returns_empty_string() {
    let result = init("   \n  \t  ", false, 0.8, 0.7, 0.4);
    assert!(
        result.is_empty(),
        "whitespace-only input should produce empty output"
    );
}
