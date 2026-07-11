use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA};

use super::super::MarkdownRenderable;
use super::super::MarkdownPalette;

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
    let has_fn = (0..10).any(|row| (0..60).any(|col| buf.cell((col, row)).map_or(false, |c| c.symbol() == "f")));
    assert!(has_fn, "'fn' should be rendered somewhere in the code block");
}

#[test]
fn test_blockquote_muted_text() {
    let md = make_md("> quoted text");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
    md.render_self(&mut buf, Rect::new(0, 0, 40, 5));

    let palette = MarkdownPalette::new(RGBA::from_ints(220, 220, 220, 255), RGBA::from_ints(0, 0, 0, 0));
    let muted_color = rgba_to_color(palette.muted_color());
    assert_eq!(
        buf.cell((0, 0)).unwrap().style().fg,
        Some(muted_color),
        "Blockquote text should be muted"
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
    assert!(buf.cell((1, 1)).unwrap().style().add_modifier.contains(Modifier::BOLD));

    // Header/body separator
    assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "├");
    assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "┼");
    assert_eq!(buf.cell((7, 2)).unwrap().symbol(), "┤");

    // Body row
    assert_eq!(buf.cell((1, 3)).unwrap().symbol(), "1");
    assert_eq!(buf.cell((5, 3)).unwrap().symbol(), "2");
    assert!(!buf.cell((1, 3)).unwrap().style().add_modifier.contains(Modifier::BOLD));

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
    let palette = MarkdownPalette::new(RGBA::from_ints(220, 220, 220, 255), RGBA::from_ints(0, 0, 0, 0));
    let muted_color = rgba_to_color(palette.muted_color());
    assert_eq!(
        buf.cell((0, 0)).unwrap().style().fg,
        Some(muted_color),
        "Default table border should use muted colour"
    );
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
