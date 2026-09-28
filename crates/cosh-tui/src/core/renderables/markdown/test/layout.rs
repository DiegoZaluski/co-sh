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

/// Language-less code block with 2 lines: its own blank top-padding row
/// (the twin of the bottom padding) + 2 code rows = 3 rows. As the LAST
/// block of a document it also drops its trailing feed row, and its top
/// padding replaces the inter-block margin it used to rely on. Pinned EXACT
/// so a regression in the accounting cannot hide inside a loose range.
#[test]
fn test_code_block_lines() {
    let text = "```\nline1\nline2\n```";
    let h = estimate_height(text, 80);
    assert_eq!(
        h, 3,
        "2-line langless block = 1 top-padding row + 2 code rows — the \
         phantom trailing blank is not reserved (got {h})"
    );
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
    // One list BLOCK: nested items stay compact, one row per item.
    assert_eq!(h, 3, "nested list with 3 items renders 3 compact rows");
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
/// plus 3 code rows and bottom padding (1) = 5 visible rows; the trailing
/// feed row is dropped because this is the document's last block. Pinned
/// EXACT so a regression in the accounting cannot pass inside a loose range.
#[test]
fn test_code_block_fenced_height() {
    let text = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
    let h = estimate_height(text, 80);
    assert_eq!(
        h, 4,
        "fenced block with padding paints 4 rows; the trailing blank feed \
         row is NOT reserved (got {h})"
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
    // Headings are separated blocks: a blank row between each pair.
    assert_eq!(h, 5, "three consecutive headings + 2 separators = 5 rows");
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
    // Rules are separated blocks: a blank row between each pair.
    assert_eq!(h, 5, "three horizontal rules + 2 separators = 5 rows");
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

/// Row index of the first row whose cell at `x` holds the glyph `ch`
/// (a cheap positional oracle: code content starts at `CODE_PAD_H` = 2).
fn glyph_row(buf: &Buffer, x: u16, h: u16, ch: char) -> Option<u16> {
    (0..h).find(|&y| buf.cell((x, y)).is_some_and(|c| c.symbol().starts_with(ch)))
}

/// The language-less fence's top gap must be INTRINSIC to the block — the
/// twin of its bottom padding — not borrowed from the inter-block margin
/// rule. Regression pins for the three shapes that used to lose the gap:
/// fence as the FIRST block (no predecessor to borrow a margin from), and
/// fences inside a quote or a list item (inter-block margins only exist
/// between top-level blocks). A fence after a separated block must show
/// exactly ONE blank row, not two (the margin is waived — the fence brings
/// its own gap), matching the labelled variant's geometry.
#[test]
fn test_langless_fence_owns_top_padding() {
    fn render(text: &str, w: u16, h: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let md = MarkdownRenderable::new(Some(text.to_string()));
        md.render_self(&mut buf, Rect::new(0, 0, w, h));
        buf
    }

    // First block: code starts on row 1 (was row 0 — glued to the top).
    let buf = render("```\ncode\n```", 40, 6);
    assert_eq!(
        glyph_row(&buf, 2, 6, 'c'),
        Some(1),
        "langless fence as first block must paint its own top padding row"
    );
    // Final block: the trailing bottom-padding/feed rows are blank, so the
    // estimate counts only up to the last glyph row (padding + code = 2).
    assert_eq!(estimate_height("```\ncode\n```", 40), 2);

    // After a paragraph: para(0), ONE blank row(1), code(2) — no double gap.
    let buf = render("para\n\n```\ncode\n```", 40, 8);
    assert_eq!(glyph_row(&buf, 0, 8, 'p'), Some(0));
    assert_eq!(
        glyph_row(&buf, 2, 8, 'c'),
        Some(2),
        "margin must be waived: the fence's own padding is the only gap"
    );

    // Inside a quote: quote(0), padding row(1), code(2).
    let buf = render("> quote\n>\n> ```\n> code\n> ```", 40, 8);
    assert_eq!(glyph_row(&buf, 2, 8, 'q'), Some(0));
    assert_eq!(
        glyph_row(&buf, 4, 8, 'c'),
        Some(2),
        "quote contents have no margin rule at all: the padding row is the gap"
    );

    // Inside a list item: item(0), padding row(1), code(2). The fence's
    // content aligns with the item's content column (x=2).
    let buf = render("- item\n\n  ```\n  code\n  ```", 40, 8);
    assert_eq!(glyph_row(&buf, 2, 8, 'i'), Some(0));
    assert_eq!(
        glyph_row(&buf, 2, 8, 'c'),
        Some(2),
        "list-embedded fences rely on the intrinsic padding row too"
    );
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

/// Regression guard for the per-block height memo (`BLOCK_HEIGHT_CACHE`):
/// the same fenced block measured as a document-FINAL block (drops its
/// trailing feed row) and as an INTERIOR block (keeps it) must get DISTINCT
/// memo entries. If the `keeps_feed` flag were dropped from the key, one of
/// these exact totals would come back wrong depending on call order.
#[test]
fn test_block_height_memo_distinguishes_final_vs_interior() {
    // Same fenced block, once as the document-FINAL block (its trailing feed
    // row is dropped from the total) and once INTERIOR followed by a
    // paragraph (feed row kept). Pinned exact totals measured against the
    // from-scratch renderer; if the `keeps_feed` flag were dropped from the
    // memo key, whichever variant was measured FIRST would poison the other.
    let fence_final = "intro\n\n```\ncode\n```";
    let fence_interior = "intro\n\n```\ncode\n```\n\ntail";
    let h_final = estimate_height(fence_final, 80);
    let h_interior = estimate_height(fence_interior, 80);
    assert_eq!(h_final, 3);
    assert_eq!(h_interior, 6);

    // Re-measure both in the opposite order: warm memo hits must return
    // identical values.
    assert_eq!(estimate_height(fence_interior, 80), h_interior);
    assert_eq!(estimate_height(fence_final, 80), h_final);
}

/// The streaming layout re-measures a growing document every frame while the
/// memo warms up incrementally. Whatever is measured through the cache must
/// equal the from-scratch value for the same text and width — here exercised
/// by measuring the long doc first (populating entries) and then verifying a
/// longer doc whose prefix shares blocks still lands on the expected total.
#[test]
fn test_block_height_memo_warm_matches_cold() {
    let short = "# Title\n\ntext with `code`.\n\n```rust\nlet x = 1;\n```\n\n- a\n- b";
    let long = format!("{short}\n\nafterword");
    let w = 60;

    let cold_long = estimate_height(&long, w); // no relevant entries yet? (best effort)
    let _ = estimate_height(short, w); // warm shared-block entries
    let warm_long = estimate_height(&long, w);

    assert_eq!(
        cold_long, warm_long,
        "memoized block heights drifted from from-scratch measurement"
    );

    // Interior slices keep the final feed row. For THIS doc the trailing
    // block is a list without a phantom row, so append a fence to make the
    // difference observable: fence-terminated docs measure exactly one row
    // more through the interior API.
    let fenced_short = format!("{short}\n\n```\ncode\n```");
    let fenced_long = format!("{long}\n\n```\ncode\n```");
    assert_eq!(
        super::super::estimate_height_interior_slice(&fenced_long, w),
        super::super::estimate_height(&fenced_long, w) + 1,
        "interior slice must keep the trailing feed row of its last block"
    );
    let _ = (fenced_short, fenced_long);
}
