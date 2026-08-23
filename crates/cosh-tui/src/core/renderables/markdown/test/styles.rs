use crate::core::rgba::RGBA;

use super::super::MarkdownPalette;
use super::super::context::MarkdownElement;

#[test]
fn test_palette_construction() {
    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let palette = MarkdownPalette::new(text, bg);

    // Headings should be brighter/different from plain text
    let heading_style = palette.style_for(None, Some(1));
    assert!(
        heading_style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "H1 should be bold"
    );

    // Emphasis should add italic
    let emph_style = palette.style_for(Some(MarkdownElement::Emphasis), None);
    assert!(
        emph_style
            .add_modifier
            .contains(ratatui::style::Modifier::ITALIC),
        "Emphasis should be italic"
    );

    // Strong should add bold
    let strong_style = palette.style_for(Some(MarkdownElement::Strong), None);
    assert!(
        strong_style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "Strong should be bold"
    );

    // Link should be underlined
    let link_style = palette.style_for(Some(MarkdownElement::Link), None);
    assert!(
        link_style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
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
    if let (
        Some(ratatui::style::Color::Rgb(r1, g1, b1)),
        Some(ratatui::style::Color::Rgb(r2, g2, b2)),
    ) = (h1_style.fg, h6_style.fg)
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

#[test]
fn test_accent_overrides_heading_ramp() {
    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let mut palette = MarkdownPalette::new(text, bg);
    let heading = RGBA::from_ints(255, 126, 219, 255); // hot pink
    palette.set_accent_colors(super::super::MarkdownAccentColors {
        heading: Some(heading),
        ..Default::default()
    });

    // H1 uses the theme color directly.
    let h1 = palette.style_for(None, Some(1));
    assert_eq!(h1.fg, Some(ratatui::style::Color::Rgb(255, 126, 219)));
    // H6 lands on the derived muted color.
    let h6 = palette.style_for(None, Some(6));
    assert_ne!(h6.fg, h1.fg, "H6 must not reuse the accent verbatim");
}

#[test]
fn test_accent_overrides_inline_elements() {
    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let mut palette = MarkdownPalette::new(text, bg);
    palette.set_accent_colors(super::super::MarkdownAccentColors {
        emph: Some(RGBA::from_ints(255, 201, 77, 255)),
        strong: Some(RGBA::from_ints(255, 255, 255, 255)),
        inline_code_fg: Some(RGBA::from_ints(255, 209, 240, 255)),
        link_label: Some(RGBA::from_ints(255, 157, 230, 255)),
        list_enumeration: Some(RGBA::from_ints(100, 240, 220, 255)),
        ..Default::default()
    });

    let emph = palette.style_for(Some(MarkdownElement::Emphasis), None);
    assert_eq!(emph.fg, Some(ratatui::style::Color::Rgb(255, 201, 77)));
    assert!(emph.add_modifier.contains(ratatui::style::Modifier::ITALIC));

    let strong = palette.style_for(Some(MarkdownElement::Strong), None);
    assert_eq!(strong.fg, Some(ratatui::style::Color::Rgb(255, 255, 255)));

    let code = palette.style_for(Some(MarkdownElement::InlineCode), None);
    assert_eq!(code.fg, Some(ratatui::style::Color::Rgb(255, 209, 240)));
    assert!(code.bg.is_some(), "inline code keeps its derived bg");

    // Concealed label: themed color + underline.
    let label = palette.link_label_style(false);
    assert_eq!(label.fg, Some(ratatui::style::Color::Rgb(255, 157, 230)));
    assert!(
        label
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );

    // Ordered numbers take their own color; bullets keep falling back to
    // the derived marker (text) color.
    assert_eq!(
        palette.list_enumeration_color(),
        RGBA::from_ints(100, 240, 220, 255)
    );
    assert_eq!(palette.list_marker_color(), text);
}

#[test]
fn test_unspecified_accents_keep_derived_defaults() {
    let text = RGBA::from_ints(200, 200, 200, 255);
    let bg = RGBA::from_ints(0, 0, 0, 255);
    let plain = MarkdownPalette::new(text, bg);
    let mut partial = MarkdownPalette::new(text, bg);
    partial.set_accent_colors(super::super::MarkdownAccentColors {
        horizontal_rule: Some(RGBA::from_ints(1, 2, 3, 255)),
        ..Default::default()
    });

    // Only the supplied element changes; everything else matches derivation.
    assert_eq!(
        partial.horizontal_rule_color(),
        RGBA::from_ints(1, 2, 3, 255)
    );
    assert_eq!(partial.table_header_color(), plain.table_header_color());
    assert_eq!(
        partial.list_enumeration_color(),
        plain.list_marker_color(),
        "enumeration falls back to the marker color"
    );
}

#[test]
fn test_syntax_colors_map_to_categories() {
    use cosh_sdk::tree_sitter::highlight::HighlightCategory;

    let text = RGBA::from_ints(220, 220, 220, 255);
    let bg = RGBA::from_ints(10, 10, 10, 255);
    let plain = MarkdownPalette::new(text, bg);
    assert!(
        plain.syntax_color(HighlightCategory::Keyword).is_none(),
        "without overrides categories fall back to built-ins"
    );

    let mut palette = MarkdownPalette::new(text, bg);
    palette.set_syntax_colors(super::super::SyntaxColors {
        keyword: Some(RGBA::from_ints(157, 140, 255, 255)),
        string: Some(RGBA::from_ints(255, 201, 77, 255)),
        comment: Some(RGBA::from_ints(86, 90, 120, 255)),
        function: Some(RGBA::from_ints(130, 170, 255, 255)),
        number: Some(RGBA::from_ints(255, 126, 219, 255)),
        r#type: Some(RGBA::from_ints(199, 146, 234, 255)),
        builtin: Some(RGBA::from_ints(100, 240, 220, 255)),
    });

    assert_eq!(
        palette.syntax_color(HighlightCategory::Keyword),
        Some(RGBA::from_ints(157, 140, 255, 255))
    );
    assert_eq!(
        palette.syntax_color(HighlightCategory::Builtin),
        Some(RGBA::from_ints(100, 240, 220, 255))
    );
}
