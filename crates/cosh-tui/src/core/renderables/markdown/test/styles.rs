use crate::core::rgba::RGBA;

use super::super::context::MarkdownElement;
use super::super::MarkdownPalette;

#[test]
fn test_palette_construction() {
    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let palette = MarkdownPalette::new(text, bg);

    // Headings should be brighter/different from plain text
    let heading_style = palette.style_for(None, Some(1));
    assert!(
        heading_style.add_modifier.contains(ratatui::style::Modifier::BOLD),
        "H1 should be bold"
    );

    // Emphasis should add italic
    let emph_style = palette.style_for(Some(MarkdownElement::Emphasis), None);
    assert!(
        emph_style.add_modifier.contains(ratatui::style::Modifier::ITALIC),
        "Emphasis should be italic"
    );

    // Strong should add bold
    let strong_style = palette.style_for(Some(MarkdownElement::Strong), None);
    assert!(
        strong_style.add_modifier.contains(ratatui::style::Modifier::BOLD),
        "Strong should be bold"
    );

    // Link should be underlined
    let link_style = palette.style_for(Some(MarkdownElement::Link), None);
    assert!(
        link_style.add_modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "Link should be underlined"
    );

    // Inline code should have background
    let code_style = palette.style_for(Some(MarkdownElement::InlineCode), None);
    assert!(
        code_style.bg.is_some(),
        "Inline code should have a background"
    );
}

#[test]
fn test_heading_inline_combination() {
    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let palette = MarkdownPalette::new(text, bg);

    // ***bold text*** inside H2 → both bold (from heading) and bold (from strong)
    let style = palette.style_for(Some(MarkdownElement::Strong), Some(2));
    assert!(
        style.add_modifier.contains(ratatui::style::Modifier::BOLD),
        "Combined heading + strong should be bold"
    );
}

#[test]
fn test_h1_is_brighter_than_h6() {
    let text = RGBA::from_ints(200, 200, 200, 255);
    let bg = RGBA::from_ints(0, 0, 0, 255);
    let palette = MarkdownPalette::new(text, bg);

    let h1_style = palette.style_for(None, Some(1));
    let h6_style = palette.style_for(None, Some(6));

    // H1 should be brighter (numerically larger RGB values) than H6
    if let (Some(ratatui::style::Color::Rgb(r1, g1, b1)), Some(ratatui::style::Color::Rgb(r2, g2, b2))) =
        (h1_style.fg, h6_style.fg)
    {
        let lum1 = r1 as u32 + g1 as u32 + b1 as u32;
        let lum2 = r2 as u32 + g2 as u32 + b2 as u32;
        assert!(
            lum1 > lum2,
            "H1 should be brighter than H6 ({} vs {})",
            lum1,
            lum2
        );
    }
}
