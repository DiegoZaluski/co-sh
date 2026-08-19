use super::super::MarkdownRenderable;
use super::super::estimate_height;
use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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

/// Language-less code block with 2 lines: no internal top-gap row — the only
/// top spacing is the margin row left by the previous block — so the height
/// is 2 code rows + bottom padding (1) + blank separator (1) + TagEnd blank
/// (1) = 5 rows. Pinned EXACT so a regression in the accounting cannot hide
/// inside a loose range.
#[test]
fn test_code_block_lines() {
    let text = "```\nline1\nline2\n```";
    let h = estimate_height(text, 80);
    assert_eq!(h, 5, "2-line code block should estimate 5 rows (got {h})");
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
    let valid = (2..=5).contains(&h);
    assert!(
        valid,
        "two blockquote lines should estimate 2-5 lines (got {h})"
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

/// Fenced code block with a language tag and 3 code lines: the label row (1)
/// plus 3 code rows, bottom padding (1), a blank separator (1), and a TagEnd
/// blank (1) = 7 rows. Pinned EXACT so a regression in the accounting (e.g.
/// the N+2 vs N+4 mistake) cannot pass inside a loose range.
#[test]
fn test_code_block_fenced_height() {
    let text = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
    let h = estimate_height(text, 80);
    assert_eq!(
        h, 7,
        "3-line fenced code block should estimate 7 rows (got {h})"
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

/// Count rows from `y=0` that contain at least one non-space glyph (the same
/// "content height" oracle the TUI uses).
fn scan_glyph_rows(buf: &Buffer, w: u16, h: u16) -> u16 {
    let mut last_row: Option<u16> = None;
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = buf.cell((x, y))
                && cell.symbol().chars().next().unwrap_or(' ') != ' '
            {
                last_row = Some(y);
                break;
            }
        }
    }
    last_row.map(|r| r + 1).unwrap_or(0)
}

/// Pins `estimate_height` against the ACTUAL rendered rows for code blocks
/// across widths. A divergence between the two — e.g. an estimate of 7 rows
/// for content that renders in 27 — silently breaks the chat layout (clipped
/// or overlapping content), and the loose single-value layout tests above
/// cannot see it. This test is the false-positive guard: it compares the two
/// implementations directly, so any change to the renderer OR the estimator
/// that drifts apart fails here.
#[test]
fn test_code_block_estimate_matches_render() {
    let cases: &[&str] = &[
        // No language tag (the reported case: asymmetric padding).
        "```\nlet a = 1;\nlet b = 2;\n```",
        // Language tag (top gap row carries the label).
        "```rust\nfn main() {}\n```",
        "```python\ndef f():\n    return 1\n```",
        // Text BEFORE and AFTER the block — the clipping regression.
        "Some text.\n\n```\ncode\n```\n\nThe end.",
        "```json\n{\"a\": 1}\n```\n\nFinal paragraph.",
        // A long unbroken code line that must wrap.
        "```\nlet very_long = \"this_is_a_very_long_line_with_no_spaces_that_wraps_inside_the_code_block\";\n```",
    ];
    for text in cases {
        for w in [14u16, 40, 80, 120] {
            let est = estimate_height(text, w).max(1);
            // Tall buffer: the renderer also paints padding rows beyond the
            // glyph height, so give it plenty of room before scanning.
            let h = est.saturating_add(40);
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            let mut md = MarkdownRenderable::new(Some(text.to_string()));
            md.set_fg(Some(ColorInput::RGBA(RGBA::from_ints(220, 220, 220, 255))));
            md.set_bg(Some(ColorInput::RGBA(RGBA::from_ints(0, 0, 0, 0))));
            md.render_self(&mut buf, Rect::new(0, 0, w, h));
            let actual = scan_glyph_rows(&buf, w, h);
            // The estimate must never be SMALLER than the rendered glyph
            // height: an under-estimate clips the message in the chat. (A
            // block at the very end of the text over-estimates by the blank
            // padding rows, which the glyph scan does not count — harmless.)
            assert!(
                est >= actual,
                "estimate {est} < rendered {actual} rows for {text:?} at max_w={w} \
                 (an under-estimate clips the chat message)"
            );
        }
    }
}
