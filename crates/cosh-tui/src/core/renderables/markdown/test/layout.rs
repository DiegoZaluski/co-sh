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
    assert_eq!(
        h1, h2,
        "ordered and unordered with same content should have same height"
    );
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
    assert!(
        h == 2 || h == 3,
        "two blockquote lines = 2-3 lines (got {h})"
    );
}

#[test]
fn test_heading() {
    let text = "# Title\n\nParagraph";
    let h = estimate_height(text, 80);
    assert!(h >= 2, "heading + paragraph should be at least 2 lines");
}

#[test]
fn test_task_list_height() {
    let text = "- [x] done\n- [ ] pending";
    let h = estimate_height(text, 80);
    assert_eq!(h, 2, "two task list items = 2 lines");
}

#[test]
fn test_nested_list_height() {
    // pulldown-cmark requires 4-space indent for nested lists
    let text = "- outer\n    - inner1\n    - inner2";
    let h = estimate_height(text, 80);
    assert_eq!(h, 4, "nested list with 3 items + paragraph break = 4 lines");
}

#[test]
fn test_wide_table_needs_multiple_rows() {
    // Table with 5 columns at narrow width forces wrapping
    let text = "| A | B | C | D | E |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |\n";
    let h = estimate_height(text, 20);
    // Should include top border + header + separator + body + bottom border = at least 5
    assert!(
        h >= 5,
        "Wide table should estimate at least 5 rows, got {h}"
    );
}

#[test]
fn test_code_block_fenced_height() {
    let text = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
    let h = estimate_height(text, 80);
    // Code block: newline before(0 or 1) + 4 lines of code + newline after(0 or 1)
    assert!(
        h >= 3,
        "Fenced code block with 4 lines should be >= 3, got {h}"
    );
}

#[test]
fn test_content_wraps_at_narrow_width() {
    let long_word = "a".repeat(100);
    let h = estimate_height(&long_word, 10);
    // 100 chars at 10 wide = 10 lines
    assert_eq!(h, 10, "100 chars at 10 wide should need 10 lines, got {h}");
}

#[test]
fn test_consecutive_headings_height() {
    let text = "# H1\n## H2\n### H3";
    let h = estimate_height(text, 80);
    assert_eq!(h, 3, "three consecutive headings = 3 lines");
}

#[test]
fn test_hard_break_height() {
    let text = "line1\\\nline2";
    // CommonMark: backslash + newline = hard break
    let h = estimate_height(text, 80);
    assert!(h >= 2, "hard break should produce 2+ lines, got {h}");
}

#[test]
fn test_mixed_content_height() {
    let text = "# Title\n\nSome paragraph text here.\n\n- list item 1\n- list item 2\n\n```\ncode block\n```\n\n> blockquote";
    let h = estimate_height(text, 80);
    assert!(
        h >= 7,
        "Mixed content should estimate at least 7 lines, got {h}"
    );
}

#[test]
fn test_table_without_data_rows() {
    let text = "| H1 | H2 |\n|---|---|";
    let h = estimate_height(text, 80);
    // top + header + separator + bottom = 4
    assert_eq!(
        h, 4,
        "Empty table (headers only) should estimate 4 lines, got {h}"
    );
}

#[test]
fn test_horizontal_rules_sequence() {
    let text = "---\n\n---\n\n---";
    let h = estimate_height(text, 80);
    assert_eq!(h, 3, "three horizontal rules = 3 lines");
}
