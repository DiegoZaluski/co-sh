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
fn test_code_block_no_lang_fallback() {
    // Code block without language specifier - should fall back to javascript and render
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

    // H1 on row 0
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "1");
    // H2 on row 1
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "2");
    // H3 on row 2
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "H");
    assert_eq!(buf.cell((1, 2)).unwrap().symbol(), "3");
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
    assert!(
        buf.cell((0, 1))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "H6 should be bold"
    );
    // H1 should be brighter than H6
    if let (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) = (
        buf.cell((0, 0)).unwrap().style().fg,
        buf.cell((0, 1)).unwrap().style().fg,
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
