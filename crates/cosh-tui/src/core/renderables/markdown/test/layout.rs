use super::super::estimate_height;

#[test]
fn test_plain_text() {
    let h = estimate_height("hello world", 80);
    assert_eq!(h, 1, "short text should fit on one line");
}

#[test]
fn test_text_wrapping() {
    let text = "a".repeat(200);
    let h = estimate_height(&text, 80);
    assert_eq!(h, 3, "200 chars at 80 wide = 3 lines");
}

#[test]
fn test_code_block_lines() {
    let text = "```\nline1\nline2\n```";
    let h = estimate_height(text, 80);
    assert!(h == 2 || h == 4, "code block with 2 lines (got {h})");
}

#[test]
fn test_ordered_list() {
    let text = "1. first\n2. second";
    let h = estimate_height(text, 80);
    assert_eq!(h, 2, "two list items = 2 lines");
}

#[test]
fn test_unordered_list() {
    let text = "- first\n- second";
    let h = estimate_height(text, 80);
    assert_eq!(h, 2, "two unordered list items = 2 lines");
}

#[test]
fn test_mixed_lists() {
    let text = "- a\n- b";
    let h1 = estimate_height(text, 80);
    let text2 = "1. a\n2. b";
    let h2 = estimate_height(text2, 80);
    assert_eq!(h1, h2, "ordered and unordered with same content should have same height");
}

#[test]
fn test_rule() {
    let h = estimate_height("---", 80);
    assert_eq!(h, 1, "horizontal rule is one line");
}

#[test]
fn test_empty() {
    let h = estimate_height("", 80);
    assert_eq!(h, 1, "empty markdown should return 1");
}

#[test]
fn test_zero_width() {
    let h = estimate_height("hello", 0);
    assert_eq!(h, 1, "zero width should return 1");
}

#[test]
fn test_blockquote() {
    let text = "> line 1\n> line 2";
    let h = estimate_height(text, 80);
    assert!(h == 2 || h == 3, "two blockquote lines = 2-3 lines (got {h})");
}

#[test]
fn test_heading() {
    let text = "# Title\n\nParagraph";
    let h = estimate_height(text, 80);
    assert!(h >= 2, "heading + paragraph should be at least 2 lines");
}
