use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::markdown::{
    MarkdownAccentColors, MarkdownRenderable, SyntaxColors,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::theme::Theme;

/// Build the accent-color set for markdown elements from a theme.
#[must_use]
pub fn accent_colors(theme: &Theme) -> MarkdownAccentColors {
    MarkdownAccentColors {
        heading: Some(theme.markdown_heading),
        link: Some(theme.markdown_link),
        link_label: Some(theme.markdown_link_text),
        inline_code_fg: Some(theme.markdown_code),
        blockquote_bar: Some(theme.markdown_block_quote),
        emph: Some(theme.markdown_emph),
        strong: Some(theme.markdown_strong),
        horizontal_rule: Some(theme.markdown_horizontal_rule),
        list_marker: Some(theme.markdown_list_item),
        list_enumeration: Some(theme.markdown_list_enumeration),
        table_header: Some(theme.markdown_table_header),
    }
}

/// Build the syntax-highlighting color set for code blocks from a theme.
#[must_use]
pub fn syntax_colors(theme: &Theme) -> SyntaxColors {
    SyntaxColors {
        comment: Some(theme.syntax_comment),
        keyword: Some(theme.syntax_keyword),
        function: Some(theme.syntax_function),
        string: Some(theme.syntax_string),
        number: Some(theme.syntax_number),
        r#type: Some(theme.syntax_type),
        builtin: Some(theme.syntax_operator),
    }
}

/// Apply every theme-driven color knob to a markdown renderable: accents,
/// code-block highlighting and the themed table border.
pub fn apply_theme(md: &mut MarkdownRenderable, theme: &Theme) {
    md.set_accent_colors(Some(accent_colors(theme)));
    md.set_syntax_colors(Some(syntax_colors(theme)));
    md.set_table_border_color(Some(ColorInput::RGBA(theme.markdown_table_border)));
}

#[allow(clippy::too_many_arguments)]
pub fn render_markdown(buf: &mut Buffer, area: Rect, content: &str, fg: RGBA, bg: RGBA) {
    if content.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let mut md = MarkdownRenderable::new(Some(content.to_string()));
    md.set_fg(Some(ColorInput::RGBA(fg)));
    md.set_bg(Some(ColorInput::RGBA(bg)));
    md.render_self(buf, area);
}
