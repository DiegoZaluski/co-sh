use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA};

use super::super::MarkdownPalette;
use super::super::MarkdownRenderable;

fn make_md(content: &str) -> MarkdownRenderable {
    let mut md = MarkdownRenderable::new(Some(content.to_string()));
    md.set_fg(Some(ColorInput::RGBA(RGBA::from_ints(220, 220, 220, 255))));
    md.set_bg(Some(ColorInput::RGBA(RGBA::from_ints(0, 0, 0, 0))));
    md
}

fn rgba_to_color(c: RGBA) -> Color {
    let (r, g, b, _) = c.to_ints();
    Color::Rgb(r, g, b)
}

#[test]
fn test_plain_text_renders() {
    let md = make_md("Hello, world!");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "e");
}

#[test]
fn test_heading_bold() {
    let md = make_md("# Title");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::BOLD),
        "Heading should be bold"
    );
}

#[test]
fn test_emphasis_italic() {
    let md = make_md("*italic*");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::ITALIC),
        "Emphasis should be italic"
    );
}

#[test]
fn test_strong_bold() {
    let md = make_md("**bold**");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::BOLD),
        "Strong should be bold"
    );
}

#[test]
fn test_inline_code_style() {
    let md = make_md("`code`");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().bg.is_some(),
        "Inline code should have background"
    );
}

#[test]
fn test_list_ordered_vs_unordered() {
    // Ordered list
    let md_ordered = make_md("1. first\n2. second");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md_ordered.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // Should show "1. " at the start
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), ".");
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), " ");

    // Unordered list
    let md_unordered = make_md("- first\n- second");
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 40, 10));
    md_unordered.render_self(&mut buf2, Rect::new(0, 0, 40, 10));

    // Should show "• " at the start (bullet, not number)
    let cell = buf2.cell((0, 0)).unwrap();
    assert_eq!(cell.symbol(), "•");
}

#[test]
fn test_code_block_syntax_highlighting() {
    let md = make_md("```rust\nfn main() {}\n```\n");
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 60, 10));

    // Verify that the code block renders characters
    let has_fn = (0..10)
        .any(|row| (0..60).any(|col| buf.cell((col, row)).is_some_and(|c| c.symbol() == "f")));
    assert!(
        has_fn,
        "'fn' should be rendered somewhere in the code block"
    );
}

#[test]
fn test_blockquote_muted_text() {
    let md = make_md("> quoted text");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Blockquote content starts at row 0, column 2 (indent)
    // with no explicit fg (inherits from container), no bold, no bg.
    assert_eq!(
        buf.cell((2, 0)).unwrap().style().fg,
        Some(Color::Reset),
        "Blockquote text should have no explicit fg (inherits from container)"
    );
    assert!(
        !buf.cell((2, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "Blockquote text should NOT be bold"
    );
}

#[test]
fn test_horizontal_rule() {
    let md = make_md("---");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "─");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "─");
}

#[test]
fn test_empty_content() {
    let md = make_md("");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));
}

#[test]
fn test_text_wrapping() {
    let long_text = "a".repeat(100);
    let md = make_md(&long_text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "a");
    assert_eq!(buf.cell((29, 0)).unwrap().symbol(), "a");
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "a");
}

#[test]
fn test_simple_table_renders() {
    let text = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    // Top border: col_starts=[0,4], cw=3 each. Corners at (0,3,7)
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "┌");
    assert_eq!(buf.cell((3, 0)).unwrap().symbol(), "┬");
    assert_eq!(buf.cell((7, 0)).unwrap().symbol(), "┐");

    // Header row: content at sx+padding
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "A");
    assert_eq!(buf.cell((5, 1)).unwrap().symbol(), "B");
    assert!(
        buf.cell((1, 1))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD)
    );

    // Header/body separator
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "├");
    assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "┼");
    assert_eq!(buf.cell((7, 2)).unwrap().symbol(), "┤");

    // Body row
    assert_eq!(buf.cell((1, 3)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((5, 3)).unwrap().symbol(), "2");
    assert!(
        !buf.cell((1, 3))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD)
    );

    // Bottom border
    assert_eq!(buf.cell((0, 4)).unwrap().symbol(), "└");
    assert_eq!(buf.cell((3, 4)).unwrap().symbol(), "┴");
    assert_eq!(buf.cell((7, 4)).unwrap().symbol(), "┘");
}

#[test]
fn test_table_with_multiple_rows() {
    let text = "| H1 | H2 | H3 |\n|---|---|---|\n| a | b | c |\n| d | e | f |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    // Header
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "H");
    // First body row
    assert_eq!(buf.cell((1, 3)).unwrap().symbol(), "a");
    // Second body row
    assert_eq!(buf.cell((1, 4)).unwrap().symbol(), "d");
}

#[test]
fn test_table_border_color() {
    let text = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let mut md = make_md(text);
    // Set a distinctive red colour for table borders
    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(255, 0, 0, 255))));

    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    // Top-left corner should be red (border colour)
    let cell = buf.cell((0, 0)).unwrap();
    assert_eq!(
        cell.style().fg,
        Some(Color::Rgb(255, 0, 0)),
        "Table border should use custom colour"
    );

    // Header text should NOT be red (should stay as default text colour)
    let header_cell = buf.cell((1, 1)).unwrap();
    assert_ne!(
        header_cell.style().fg,
        Some(Color::Rgb(255, 0, 0)),
        "Header text should not inherit border colour"
    );
}

#[test]
fn test_table_border_color_default_is_muted() {
    let text = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    // Without custom colour, border should use palette's muted colour
    let palette = MarkdownPalette::new(
        RGBA::from_ints(220, 220, 220, 255),
        RGBA::from_ints(0, 0, 0, 0),
    );
    let muted_color = rgba_to_color(palette.muted_color());
    assert_eq!(
        buf.cell((0, 0)).unwrap().style().fg,
        Some(muted_color),
        "Default table border should use muted colour"
    );
}

/// Regression: a VS16 emoji sequence ("⚙️" = U+2699 + U+FE0F) inside a table
/// cell used to be split char-by-char into TWO cells (one holding a bare
/// VS16). Terminals render the pair as one 2-column glyph while ratatui's
/// diff models the cells with mismatched widths — so when the surrounding UI
/// changed (e.g. switching sessions), the affected physical columns were
/// never repainted and the row "leaked" into other views.
///
/// The cell writer must place each grapheme cluster in ONE cell, size columns
/// by display width and mark wide-glyph shadow columns as `Skip` — exactly
/// like the prose renderer does.
#[test]
fn test_table_vs16_emoji_never_splits_into_two_cells() {
    let text = "| Grupo | Tasks |\n|---|---|\n| \u{2699}\u{FE0F} DevOps | 16 tasks |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // No cell anywhere may hold a bare variation selector.
    for y in 0..10u16 {
        for x in 0..40u16 {
            let symbol = buf.cell((x, y)).unwrap().symbol();
            assert!(
                !symbol.contains('\u{FE0F}') || symbol.len() > 1,
                "bare VS16 leaked into its own cell at ({x},{y}): {symbol:?}"
            );
        }
    }

    // The combined grapheme occupies exactly one cell...
    let emoji_cell = (0..40u16).find_map(|x| {
        (0..10u16).find_map(|y| {
            buf.cell((x, y))
                .filter(|c| c.symbol() == "\u{2699}\u{FE0F}")
                .map(|_| (x, y))
        })
    });
    let (ex, ey) = emoji_cell.expect("combined ⚙️ grapheme should occupy a single cell");

    // ...with its shadow column marked Skip so ratatui's diff can track the
    // 2-column footprint of the rendered emoji.
    let shadow = buf.cell((ex + 1, ey)).unwrap();
    assert_eq!(
        shadow.diff_option,
        ratatui::buffer::CellDiffOption::Skip,
        "wide grapheme shadow column must be marked Skip"
    );
}

/// Regression companion: column widths must be sized by display width, not
/// `chars().count()`. A single-char wide emoji (📐) is 1 char but 2 columns;
/// char counting made the column too narrow and pushed content/borders out
/// of alignment.
#[test]
fn test_table_column_width_uses_display_width() {
    // Column 1 content: "📐 X" = display width 4 (emoji=2) but 3 chars.
    let text = "| G | T |\n|---|---|\n| 📐 X | y |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 8));

    // The right border of column 1 must sit at a position that accounts for
    // the emoji's 2-column footprint: layout is [│][pad][G col][pad][│]...
    // With width-by-display-width sizing, the first data column is 2 wide
    // ("G" padded), so borders land at fixed known positions regardless.
    // Find the header separator corner on row 0: ┌ at 0, then ┬ must come
    // AFTER the full display width of column 1 (2 content + 2 padding).
    let mut sep_x = None;
    for x in 1..30u16 {
        if buf.cell((x, 0)).map(|c| c.symbol() == "┬") == Some(true) {
            sep_x = Some(x);
            break;
        }
    }
    let sep_x = sep_x.expect("table top border separator missing");
    // col_widths[0] >= max(display_width("G"), display_width("📐 X")) = 4
    // → separator sits at 1 (left pad+content+right pad) ≥ 4+2+1.
    assert!(
        sep_x >= 6,
        "column sized by chars().count() would put the separator too early (found at {sep_x})"
    );

    // The body row must keep the same border positions (no drift caused by
    // the wide glyph overflowing its column).
    for probe_y in [1u16, 3u16] {
        let body_sep = (1..30u16)
            .find(|&x| buf.cell((x, probe_y)).is_some_and(|c| c.symbol() == "│"))
            .expect("body vertical border missing");
        assert_eq!(
            body_sep, sep_x,
            "body border drifted from header separator at y={probe_y}"
        );
    }
}

#[test]
fn test_link_style() {
    let md = make_md("[link](https://example.com)");
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 60, 5));

    // Link text should be underlined
    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::UNDERLINED),
        "Link text should be underlined"
    );
}

#[test]
fn test_strikethrough_renders() {
    let md = make_md("~~struck~~");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Strikethrough text should have CROSSED_OUT modifier
    let cell = buf.cell((0, 0)).unwrap();
    assert_eq!(cell.symbol(), "s");
    assert!(
        cell.style().add_modifier.contains(Modifier::CROSSED_OUT),
        "Strikethrough text should have CROSSED_OUT modifier"
    );
}

#[test]
fn test_task_list_unchecked() {
    let md = make_md("- [ ] todo item");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Should have bullet marker
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "•");
    // Checkbox should show unchecked Unicode symbol
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "☐");
    // Text should follow
    assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "t");
}

#[test]
fn test_task_list_checked() {
    let md = make_md("- [x] done");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Checkbox should show checked Unicode symbol
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "☑");
    // Text should follow
    assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "d");
}

#[test]
fn test_task_list_multiple_items() {
    let md = make_md("- [x] step 1\n- [ ] step 2\n- [ ] step 3");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // First row: checked (☑at position 2 after bullet + space)
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "☑");
    // Second row: unchecked
    assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "☐");
    // Third row: unchecked
    assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "☐");
}

#[test]
fn test_mixed_bold_and_italic() {
    // ***text*** should apply at least bold (innermost wins)
    let md = make_md("***bold italic***");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::BOLD),
        "***text*** should have bold modifier on innermost style"
    );
}

#[test]
fn test_bold_with_inline_code() {
    let md = make_md("**bold `code` end**");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // "bold" should be bold
    let bold_cell = buf.cell((0, 0)).unwrap();
    assert!(
        bold_cell.style().add_modifier.contains(Modifier::BOLD),
        "Text before inline code in bold should be bold"
    );
    // Inline code should have background
    // "code" starts after "bold `" = 6 chars
    let code_cell = buf.cell((6, 0)).unwrap();
    assert!(
        code_cell.style().bg.is_some(),
        "Inline code inside bold should have background"
    );
}

#[test]
fn test_code_block_no_lang_renders_plain() {
    // Code block without language specifier - renders as plain text (the
    // JavaScript fallback for unknown languages was removed).
    let md = make_md("```\nfn hello() {}\n```");
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 60, 10));

    // Code block should render characters
    let has_fn = (0..10)
        .any(|row| (0..60).any(|col| buf.cell((col, row)).is_some_and(|c| c.symbol() == "f")));
    assert!(has_fn, "Code block without lang should still render");
}

#[test]
fn test_non_zero_area_offset() {
    let md = make_md("Hello\nWorld");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    // Render into a sub-area offset from origin
    md.render_self(&mut buf, Rect::new(5, 2, 20, 6));

    // Content should appear at the offset position
    assert_eq!(
        buf.cell((5, 2)).unwrap().symbol(),
        "H",
        "First line should start at area.x"
    );
    assert_eq!(
        buf.cell((5, 3)).unwrap().symbol(),
        "W",
        "Second line should start at area.x on next row"
    );
    // Content should NOT appear at origin (0,0)
    assert_eq!(
        buf.cell((0, 0)).unwrap().symbol(),
        " ",
        "Origin should remain empty (filled with background)"
    );
}

#[test]
fn test_content_truncation() {
    // Content that exceeds area height
    let md = make_md("line1\nline2\nline3\nline4\nline5");
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 3));

    // First line should render
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "l");
    // Second line should render
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "l");
    // Content beyond area height should NOT render
    // (third line wraps to y=2, fourth/fifth would be at y=3+ which is beyond max_y)
    // We can't easily check for absence at specific positions since Buffer is empty
}

#[test]
fn test_hard_break() {
    // Two spaces at end of line + newline = hard break in CommonMark
    // In pulldown-cmark, this generates HardBreak events
    let md = make_md("line1  \nline2");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "l");
    assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "1");
    // line2 should be on row 1
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "l");
    assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "2");
}

#[test]
fn test_heading_with_inline_code() {
    let md = make_md("# Install `rustup`");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Heading text should be bold
    let cell = buf.cell((0, 0)).unwrap();
    assert!(
        cell.style().add_modifier.contains(Modifier::BOLD),
        "Heading text should be bold"
    );
    // Inline code in heading should have background AND be bold (heading style)
    let code_start = "Install ".len() as u16;
    let code_cell = buf.cell((code_start, 0)).unwrap();
    assert!(
        code_cell.style().bg.is_some(),
        "Inline code in heading should have background"
    );
}

#[test]
fn test_non_zero_area_with_wrapping() {
    // Text that wraps within a sub-area
    let long_word = "hello";
    let text = format!("{} world {}", long_word, "a".repeat(30));
    let md = make_md(&text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    // Narrow area starting at x=2
    md.render_self(&mut buf, Rect::new(2, 1, 10, 5));

    // Content should start at area.x
    assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "h");
    // When text wraps, it should reset to area.x, not column 0
}

#[test]
fn test_blockquote_multiple_paragraphs() {
    let md = make_md("> First paragraph\n>\n> Second paragraph");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // First paragraph text starts at row 0, column 2 (indent).
    // No explicit fg (inherits from container), no bold, no bg.
    assert_eq!(
        buf.cell((2, 0)).unwrap().style().fg,
        Some(Color::Reset),
        "Blockquote first paragraph should have no explicit fg"
    );
}

#[test]
fn test_consecutive_headings() {
    let md = make_md("# H1\n## H2\n### H3");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // Headings are "separated" blocks: a blank row sits between each pair.
    // H1 on row 0
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "1");
    // H2 on row 2 (one blank separator at row 1)
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 2)).unwrap().symbol(), "2");
    // H3 on row 4 (one blank separator at row 3)
    assert_eq!(buf.cell((0, 4)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 4)).unwrap().symbol(), "3");
}

#[test]
fn test_table_proportional_scaling() {
    // Table wider than available width - should scale proportionally
    let text = "| A | B | C | D | E |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 10));

    // Should still render something (proportional scaling kicks in)
    // At least the top border should be visible
    let top_left = buf.cell((0, 0)).unwrap();
    assert_eq!(top_left.symbol(), "┌");
    // Some data should be visible
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "A");
}

#[test]
fn test_code_block_with_empty_lines() {
    let md = make_md("```\n\nmiddle\n\n```");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // Content should render despite empty lines
    let has_middle = (0..10)
        .any(|row| (0..40).any(|col| buf.cell((col, row)).is_some_and(|c| c.symbol() == "m")));
    assert!(
        has_middle,
        "Code block with empty lines should render 'middle'"
    );
}

#[test]
fn test_list_with_long_item_wrapping() {
    let long_item = "a".repeat(50);
    let md = make_md(&format!("- {long_item}"));
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 10));

    // Bullet marker should be at (0, 0)
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "•");
    // Text content should wrap to next line
    // At 20 wide, bullet takes 2 cols, so wrap starts at x=2
    // 50 chars should wrap multiple times
    let row1_text = (0..20).any(|col| {
        let c = buf.cell((col, 1));
        c.is_some() && c.unwrap().symbol() == "a"
    });
    assert!(row1_text, "Long list item should wrap to second line");
}

#[test]
fn test_table_custom_border_color_with_headers() {
    let text = "| Col1 | Col2 | Col3 |\n|---|---|---|\n| data1 | data2 | data3 |\n";
    let mut md = make_md(text);
    md.set_table_border_color(Some(ColorInput::RGBA(RGBA::from_ints(0, 200, 0, 255))));

    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 30, 10));

    // All border corners should use custom green color
    let green = Color::Rgb(0, 200, 0);
    assert_eq!(
        buf.cell((0, 0)).unwrap().style().fg,
        Some(green),
        "Top-left corner should be green"
    );
    assert_eq!(
        buf.cell((0, 4)).unwrap().style().fg,
        Some(green),
        "Bottom-left corner should be green"
    );
}

#[test]
fn test_heading_level_color_distinction() {
    let md = make_md("# H1\n###### H6");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 10));

    // Both headings should be bold
    assert!(
        buf.cell((0, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "H1 should be bold"
    );
    // H6 sits on row 2: a blank separator row separates the two headings.
    assert!(
        buf.cell((0, 2))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "H6 should be bold"
    );
    // H1 should be brighter than H6
    if let (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) = (
        buf.cell((0, 0)).unwrap().style().fg,
        buf.cell((0, 2)).unwrap().style().fg,
    ) {
        let lum1 = r1 as u32 + g1 as u32 + b1 as u32;
        let lum2 = r2 as u32 + g2 as u32 + b2 as u32;
        assert!(
            lum1 > lum2,
            "H1 ({lum1}) should be brighter than H6 ({lum2})"
        );
    }
}

#[test]
fn test_warning_blockquote_yellow_background() {
    // Warning text (starting with ⚠) inside a blockquote should get
    // yellow background + black fg + bold.
    let md = make_md("> ⚠ Tool call failure");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // Warning text at column 2 (indent) should have black fg (for contrast on yellow bg)
    assert_eq!(
        buf.cell((2, 0)).unwrap().style().fg,
        Some(Color::Rgb(0, 0, 0)),
        "Warning blockquote text should be black on yellow bg"
    );
    assert!(
        buf.cell((2, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "Warning blockquote text should be bold"
    );
}

#[test]
fn test_empty_content_zero_area() {
    let md = make_md("some text");
    let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
    // Should not panic when area has zero dimensions
    md.render_self(&mut buf, Rect::new(0, 0, 0, 0));
}

#[test]
fn test_soft_break_in_paragraph() {
    // Single newline within paragraph produces SoftBreak
    let md = make_md("line 1\nline 2");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    // line 1 (6 chars: l,i,n,e,' ',1) should be on row 0, '1' at index 5
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "l");
    assert_eq!(buf.cell((5, 0)).unwrap().symbol(), "1");
    // line 2 should be on row 1 (soft break advances to next line)
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "l");
    assert_eq!(buf.cell((5, 1)).unwrap().symbol(), "2");
}

/// nested lists must indent under the parent item's content column.
#[test]
fn test_nested_list_indentation() {
    let md = make_md("- outer\n    - inner");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 6));

    // Outer marker at the left edge; inner marker indented under the
    // outer content column (2).
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "•");
    assert_eq!(
        buf.cell((2, 1)).unwrap().symbol(),
        "•",
        "inner bullet should be indented to col 2"
    );
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), " ");
    // Inner content starts after the inner marker.
    assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "i");
}

/// ordered markers are right-aligned to the widest one (" 9." /
/// "10.") so all item content starts at the same column.
#[test]
fn test_ordered_markers_align_past_nine() {
    let items: Vec<String> = (1..=12).map(|i| format!("{i}. x")).collect();
    let md = make_md(&items.join("\n"));
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 14));

    for row in 0..12u16 {
        // All rows: content 'x' at the same aligned column (marker_width = 3+1).
        assert_eq!(
            buf.cell((4, row)).unwrap().symbol(),
            "x",
            "item {} content should start at column 4",
            row + 1
        );
    }
    // Right-aligned numbers: single digits get a leading space, "10." does not.
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((0, 9)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((2, 9)).unwrap().symbol(), ".");
}

/// loose-list paragraphs stay on the marker row instead of dropping
/// to the next line.
#[test]
fn test_loose_list_paragraph_on_marker_row() {
    let md = make_md("- alpha\n\n- beta");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 8));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "•");
    assert_eq!(
        buf.cell((2, 0)).unwrap().symbol(),
        "a",
        "loose item text should share the marker row"
    );
}

/// inline formatting survives into table cells — bold spans keep
/// their modifier while surrounding padding stays plain.
#[test]
fn test_table_cell_inline_styles() {
    let text = "| k |\n|---|\n| **bold** plain |\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 8));

    use ratatui::style::Modifier;
    // Body row is y=3 (top border, header, separator precede it).
    // Find the bold run: 'b' starts right after the left border + padding,
    // with column width sized to "**bold** plain" rendered as "bold plain".
    let mut found_bold_start = false;
    for x in 1..20u16 {
        if buf.cell((x, 3)).is_some_and(|c| c.symbol() == "b") {
            assert!(
                buf.cell((x, 3))
                    .unwrap()
                    .style()
                    .add_modifier
                    .contains(Modifier::BOLD),
                "'b' of bold should carry BOLD"
            );
            found_bold_start = true;
            break;
        }
    }
    assert!(found_bold_start, "bold cell content not found on body row");
}

/// Regression (review finding): marker-width slots must be consumed per list,
/// not always from index 0 — a nested ordered list with ≥10 items inside
/// another list previously fell back to width 2 and its "10."/"11." markers
/// were overwritten by item text.
#[test]
fn test_nested_ordered_list_wide_markers_get_own_slot() {
    let inner: Vec<String> = (1..=12).map(|i| format!("    {i}. x")).collect();
    let text = format!("- outer\n{}", inner.join("\n"));
    let md = make_md(&text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 30));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 30));

    // Inner content column: outer bullet width 2 + inner marker width 4.
    for row in 1..=12u16 {
        assert_eq!(
            buf.cell((6, row)).unwrap().symbol(),
            "x",
            "inner item {row} content should start at column 6"
        );
        // The number-dot separator must be intact (not overwritten by text).
        assert_eq!(
            buf.cell((4, row)).unwrap().symbol(),
            ".",
            "marker dot should survive at row {row}"
        );
    }
    // Item 10 (row 10): right-aligned two-digit marker "10. ".
    assert_eq!(buf.cell((2, 10)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((3, 10)).unwrap().symbol(), "0");
    assert_eq!(buf.cell((5, 10)).unwrap().symbol(), " ");
}

/// Regression (review finding): blockquote indentation must not be
/// double-counted for wrapped lines of a list item inside a quote.
#[test]
fn test_blockquote_list_wrap_indent_single_count() {
    let md = make_md("> - aaa bbb ccc ddd eee fff ggg hhh");
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 8));

    // First line starts at col 4 (quote indent 2 + bullet width 2).
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "•");
    // Wrapped continuation lines return to col 4 too (bar at 0, content at 4).
    let wrap_col = (2..20u16)
        .find(|&x| buf.cell((x, 1)).is_some_and(|c| c.symbol() != " "))
        .expect("wrapped continuation line expected");
    assert_eq!(
        wrap_col, 4,
        "continuation line must align with item content (past the bar), not double-count quote indent"
    );
    // The quote bar is drawn on every row of the quote.
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "│");
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "│");
}

/// Nested blockquotes accumulate one indent step per level.
#[test]
fn test_nested_blockquote_indent_accumulates() {
    let md = make_md("> level1\n>\n> > level2");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 8));

    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "l");
    assert_eq!(
        buf.cell((4, 1)).unwrap().symbol(),
        "l",
        "level-2 quote should indent to col 4"
    );
}

/// Regression (review round 2): soft-break continuation lines inside a
/// blockquote must not double-count the quote indent.
#[test]
fn test_blockquote_softbreak_indent_single_count() {
    let md = make_md("> a\n> b");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "a");
    assert_eq!(
        buf.cell((2, 1)).unwrap().symbol(),
        "b",
        "soft-broken line must stay at the quote indent (col 2), not col 4"
    );
}

/// Regression (review round 2): a quote opened INSIDE a list item must keep
/// its +2 through sibling blocks (code block → paragraph) and when opening a
/// sublist — indentation derives from open containers, never from stored
/// restores.
#[test]
fn test_quote_inside_list_item_survives_blocks() {
    let text = "- item\n\n  > ```\n  > code\n  > ```\n  >\n  > after\n";
    let md = make_md(text);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 20));

    // Find the "after" paragraph: its 'a' must sit at item content column
    // (2) plus one quote level (+2) = column 4.
    let mut after_col = None;
    for y in 0..20u16 {
        for x in 0..30u16 {
            if buf.cell((x, y)).is_some_and(|c| c.symbol() == "f")
                && buf.cell((x + 1, y)).is_some_and(|c| c.symbol() == "t")
                && buf.cell((x - 1, y)).is_some_and(|c| c.symbol() == "a")
            {
                // located an "aft" run; record start of the word
                after_col = Some(x - 1);
                break;
            }
        }
        if after_col.is_some() {
            break;
        }
    }
    assert_eq!(
        after_col,
        Some(4),
        "'after' should be indented at content+quote = col 4"
    );

    // A sublist opened inside that same quote also inherits the quote level:
    // its bullet lands at content(2)+quote(2) = 4.
    let text2 = "- item\n\n  > - inner\n";
    let md2 = make_md(text2);
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 40, 10));
    md2.render_self(&mut buf2, Rect::new(0, 0, 40, 10));
    let bullet_col = (0..20u16).find(|&x| buf2.cell((x, 1)).is_some_and(|c| c.symbol() == "•"));
    assert_eq!(
        bullet_col,
        Some(4),
        "quote-nested bullet should sit at col 4"
    );
}

/// concealed links (default) show only the label.
#[test]
fn test_link_concealed_by_default() {
    let md = make_md("[click](https://example.com)");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 4));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 4));
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "c");
    let row: String = (0..10u16)
        .map(|x| {
            buf.cell((x, 0))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert!(!row.contains('['), "concealed link must not draw brackets");
}

/// conceal=false renders links literally as [label](url).
#[test]
fn test_link_unconcealed_renders_literal_syntax() {
    let mut md = make_md("[click](https://x.io)");
    md.set_conceal(false);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 4));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 4));
    let row: String = (0..25u16)
        .map(|x| {
            buf.cell((x, 0))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        row.starts_with("[click](https://x.io)"),
        "unconcealed link should render literally, got {row:?}"
    );
}

/// rendered links are recorded as active regions covering exactly
/// the label cells, in absolute coordinates.
#[test]
fn test_active_links_hit_map() {
    let md = make_md("see [docs](https://x.io) now");
    let mut buf = Buffer::empty(Rect::new(3, 2, 60, 4));
    md.render_self(&mut buf, Rect::new(3, 2, 60, 4));

    let links = md.active_links();
    assert_eq!(links.len(), 1, "exactly one link expected");
    let link = &links[0];
    assert_eq!(link.url, "https://x.io");
    // Label starts after "see " (4 cols) from area.x=3 → col 7; width 4 ("docs").
    assert_eq!((link.y, link.x0, link.x1), (2, 7, 11));
}

/// an unsupported language renders plain text instead of falling
/// back to JavaScript highlighting.
#[test]
fn test_unknown_lang_no_js_fallback() {
    let md = make_md("```obscurelang\nlet x = 42;\n```\n");
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    md.render_self(&mut buf, Rect::new(0, 0, 60, 10));
    let has_content = (0..10u16)
        .any(|row| (0..60u16).any(|col| buf.cell((col, row)).is_some_and(|c| c.symbol() == "l")));
    assert!(
        has_content,
        "unknown-language block must still render its text"
    );
}

/// Review fix: multi-word link labels keep the whitespace between words
/// inside the hit map (no dead zones).
#[test]
fn test_link_hit_map_covers_inter_word_spaces() {
    let md = make_md("[two words](https://x.io)");
    let mut buf = Buffer::empty(Rect::new(0, 2, 40, 4));
    md.render_self(&mut buf, Rect::new(0, 2, 40, 4));

    let links = md.active_links();
    assert_eq!(links.len(), 1, "one contiguous region expected");
    let link = &links[0];
    // "two words" = 9 columns starting at 0.
    assert_eq!((link.y, link.x0, link.x1), (2, 0, 9));
}

/// Review fix: rows clipped by the viewport never become clickable.
#[test]
fn test_active_links_skip_clipped_rows() {
    let md = make_md("before\n\n[docs](https://x.io)");
    // Viewport of a single row: the label renders on row 1 — clipped away.
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 1));

    // The label lands on row >= 3 (after "before" + blank), which the
    // viewport (height 3) clips away entirely.
    assert!(
        md.active_links().is_empty(),
        "clipped link must not be clickable"
    );
}

/// blockquotes draw a muted bar and their body renders as FULL
/// markdown (inline formatting survives).
#[test]
fn test_blockquote_recursive_inline_formatting() {
    let md = make_md("> **bold** quote");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    use ratatui::style::Modifier;
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "│", "bar on first row");
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "b");
    assert!(
        buf.cell((2, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "inline bold must survive inside a recursive quote"
    );
}

/// fenced code blocks inside quotes render as code blocks.
#[test]
fn test_blockquote_contains_code_fence() {
    let md = make_md("> ```rust\n> let x = 1;\n> ```\n");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 12));

    let has_code =
        (0..12u16).any(|y| (2..40u16).any(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() == "l")));
    assert!(
        has_code,
        "code fence inside quote should render its content"
    );
    // Bar present next to the code content rows.
    let bars = (0..12u16)
        .filter(|&y| buf.cell((0, y)).is_some_and(|c| c.symbol() == "│"))
        .count();
    assert!(bars >= 3, "bar expected on multiple quote rows");
}

/// lists inside quotes get proper markers at the content column.
#[test]
fn test_blockquote_contains_list() {
    let md = make_md("> - alpha\n> - beta");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 8));

    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "•");
    assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "a");
}

/// nested quotes accumulate one bar per level via geometry.
#[test]
fn test_nested_quote_double_bar() {
    let md = make_md("> level1\n>\n> > level2");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 8));

    // Outer bar col 0; inner bar col 2; level-2 text at col 4.
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "│");
    assert_eq!(
        buf.cell((2, 1)).unwrap().symbol(),
        "│",
        "inner bar at col 2"
    );
    assert_eq!(
        buf.cell((4, 1)).unwrap().symbol(),
        "l",
        "level-2 text at col 4"
    );
}

/// the warning theme is configurable through the renderer — a custom
/// prefix triggers custom colors instead of the ⚠/yellow defaults.
#[test]
fn test_warning_theme_configurable_prefix_and_colors() {
    use ratatui::style::Color;
    let mut md = make_md("> ! danger zone");
    md.set_warning_theme(
        "!",
        RGBA::from_ints(180, 30, 30, 255),
        RGBA::from_ints(255, 255, 255, 255),
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let cell = buf.cell((2, 0)).unwrap();
    assert_eq!(cell.symbol(), "!");
    assert_eq!(
        cell.style().bg,
        Some(Color::Rgb(180, 30, 30)),
        "custom warning bg"
    );
    assert_eq!(
        cell.style().fg,
        Some(Color::Rgb(255, 255, 255)),
        "custom warning fg"
    );
}

/// Review fix: links inside blockquotes stay in the click map (translated
/// through the quote bar/content offset).
#[test]
fn test_link_inside_blockquote_is_clickable() {
    let md = make_md("> see [docs](https://x.io) here");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let links = md.active_links();
    assert_eq!(links.len(), 1, "link inside quote must be recorded");
    let link = &links[0];
    assert_eq!(link.url, "https://x.io");
    // Bar(1) + space + "see " → label starts at col 6, width 4.
    assert_eq!((link.y, link.x0, link.x1), (0, 6, 10));
}

// ── Inter-block spacing (source-semantic separators) ────────────

/// A heading is a "separated" block: the content after it starts one blank
/// row below, mirroring the reference renderer's top-level margin rule.
#[test]
fn test_heading_separated_from_following_content() {
    let md = make_md("# Title\n\nconteudo depois do titulo");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 6));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "T");
    // Row 1 is a blank separator; content starts at row 2.
    assert_eq!(
        buf.cell((0, 1)).unwrap().symbol(),
        " ",
        "blank row expected between heading and content"
    );
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "c");
}

/// Paragraphs separated by an explicit blank line in the source keep that
/// separation on screen.
#[test]
fn test_paragraphs_separated_by_source_blank_line() {
    let md = make_md("para um\n\npara dois");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 6));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "p");
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "p");
}

/// Tight lists stay compact: consecutive items occupy consecutive rows with
/// no separator between them.
#[test]
fn test_tight_list_stays_compact() {
    let md = make_md("- a\n- b\n- c");
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 5));

    for (row, ch) in [(0u16, "a"), (1, "b"), (2, "c")] {
        assert_eq!(buf.cell((2, row)).unwrap().symbol(), ch);
    }
}

/// Loose lists keep their internal spacing: the double feed from the item's
/// paragraph End plus the Item End leaves one blank row between items.
#[test]
fn test_loose_list_items_are_spaced() {
    let md = make_md("- alpha\n\n- beta");
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 6));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 6));

    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "a");
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ", "loose separator");
    assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "b");
}

/// A code fence following a paragraph gets exactly ONE blank row above it:
/// the inter-block margin. No phantom leading gap inside the fence itself.
#[test]
fn test_code_block_single_blank_above() {
    let md = make_md("text\n\n```\ncode\n```");
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 8));
    md.render_self(&mut buf, Rect::new(0, 0, 20, 8));

    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "t");
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ");
    // Code text is indented by CODE_PAD_H columns.
    assert_eq!(
        buf.cell((2, 2)).unwrap().symbol(),
        "c",
        "code starts directly after the single margin row"
    );
}

/// The LAST block of a document leaves no trailing blank row: the estimate
/// counts visible rows only (feed rows exist solely to position the next
/// block).
#[test]
fn test_no_trailing_blank_after_last_block() {
    use super::super::estimate_height;

    assert_eq!(estimate_height("hello", 40), 1);
    assert_eq!(estimate_height("# Title\n\ncontent", 40), 3);
}
