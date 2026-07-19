use ratatui::style::{Color, Modifier, Style};

use crate::core::rgba::RGBA;

use super::context::MarkdownElement;

/// Theme-aware palette that maps semantic `MarkdownElement`s to concrete
/// `ratatui::Style` values.
///
/// Mirrors \``OpenTUI`\`'s `SyntaxStyle.getStyle("markup.*")` concept:
/// each markdown construct (heading, emphasis, link, etc.) gets a
/// distinct visual treatment derived from the base text/background colors.
#[derive(Debug, Clone)]
pub struct MarkdownPalette {
    // ── Base colors ────────────────────────────────────────────
    text: RGBA,
    background: RGBA,
    muted: RGBA,

    // ── Heading colors (H1 brightest → H6 most muted) ──────────
    heading_colors: [RGBA; 6],

    // ── Inline element colors ──────────────────────────────────
    link: RGBA,
    inline_code_fg: RGBA,
    inline_code_bg: RGBA,

    // ── Block element colors ───────────────────────────────────
    code_block_bg: RGBA,
    blockquote_bg: RGBA,
    blockquote_bar: RGBA,
    list_marker: RGBA,
}

impl MarkdownPalette {
    /// Construct a palette derived from the given foreground (text) and
    /// background colours, following the same tonal logic as `OpenTUI`'s
    /// default syntax styles.
    #[must_use]
    pub fn new(text: RGBA, background: RGBA) -> Self {
        let (tr, tg, tb, _) = text.to_ints();
        let (br, bg_g, bb, _) = background.to_ints();

        // Muted = blend text toward background
        let muted = RGBA::from_ints(
            (u16::from(tr) * 3 / 4 + u16::from(br) / 4) as u8,
            (u16::from(tg) * 3 / 4 + u16::from(bg_g) / 4) as u8,
            (u16::from(tb) * 3 / 4 + u16::from(bb) / 4) as u8,
            255,
        );

        // Headings: H1 = bright/emphasized text, H6 ≈ muted
        let heading_colors = [
            RGBA::from_ints(tr, tg, tb, 255), // H1 – bright
            RGBA::from_ints(
                (u16::from(tr) * 11 / 12).min(255) as u8,
                (u16::from(tg) * 11 / 12).min(255) as u8,
                (u16::from(tb) * 11 / 12).min(255) as u8,
                255,
            ),
            RGBA::from_ints(
                (u16::from(tr) * 10 / 12).min(255) as u8,
                (u16::from(tg) * 10 / 12).min(255) as u8,
                (u16::from(tb) * 10 / 12).min(255) as u8,
                255,
            ),
            RGBA::from_ints(
                (u16::from(tr) * 9 / 12).min(255) as u8,
                (u16::from(tg) * 9 / 12).min(255) as u8,
                (u16::from(tb) * 9 / 12).min(255) as u8,
                255,
            ),
            RGBA::from_ints(
                (u16::from(tr) * 8 / 12).min(255) as u8,
                (u16::from(tg) * 8 / 12).min(255) as u8,
                (u16::from(tb) * 8 / 12).min(255) as u8,
                255,
            ),
            RGBA::from_ints(
                // H6 ≈ muted
                (u16::from(tr) * 3 / 4 + u16::from(br) / 4) as u8,
                (u16::from(tg) * 3 / 4 + u16::from(bg_g) / 4) as u8,
                (u16::from(tb) * 3 / 4 + u16::from(bb) / 4) as u8,
                255,
            ),
        ];

        // Link: cyan-ish tint on top of text color
        let link = RGBA::from_ints((tr / 3).max(80), (tg).max(180), (tb).max(220), 255);

        // Inline code: slightly off-white fg, dim bg
        let inline_code_fg = RGBA::from_ints(
            (u16::from(tr) * 9 / 10 + 25).min(255) as u8,
            (u16::from(tg) * 9 / 10 + 25).min(255) as u8,
            (u16::from(tb) * 9 / 10 + 25).min(255) as u8,
            255,
        );
        let inline_code_bg = RGBA::from_ints(
            (u16::from(br) * 3 / 4 + u16::from(tr) / 4).min(255) as u8,
            (u16::from(bg_g) * 3 / 4 + u16::from(tg) / 4).min(255) as u8,
            (u16::from(bb) * 3 / 4 + u16::from(tb) / 4).min(255) as u8,
            255,
        );

        // Code block bg = slightly different from main background
        let code_block_bg = RGBA::from_ints(
            (u16::from(br) * 9 / 10 + 8).min(255) as u8,
            (u16::from(bg_g) * 9 / 10 + 8).min(255) as u8,
            (u16::from(bb) * 9 / 10 + 8).min(255) as u8,
            255,
        );

        // Blockquote bg = solid #eef170 (pure color, no blend)
        let blockquote_bg = RGBA::from_ints(238, 241, 112, 255);

        // Blockquote bar = muted
        let blockquote_bar = muted;

        // List marker = text color
        let list_marker = text;

        Self {
            text,
            background,
            muted,
            heading_colors,
            link,
            inline_code_fg,
            inline_code_bg,
            code_block_bg,
            blockquote_bg,
            blockquote_bar,
            list_marker,
        }
    }

    // ── Queries ────────────────────────────────────────────────

    #[must_use]
    pub const fn text_color(&self) -> RGBA {
        self.text
    }

    #[must_use]
    pub const fn muted_color(&self) -> RGBA {
        self.muted
    }

    #[must_use]
    pub const fn background_color(&self) -> RGBA {
        self.background
    }

    #[must_use]
    pub const fn code_block_bg(&self) -> RGBA {
        self.code_block_bg
    }

    #[must_use]
    pub const fn blockquote_bg_color(&self) -> RGBA {
        self.blockquote_bg
    }

    #[must_use]
    pub const fn blockquote_bar_color(&self) -> RGBA {
        self.blockquote_bar
    }

    #[must_use]
    pub const fn list_marker_color(&self) -> RGBA {
        self.list_marker
    }

    /// Return the `ratatui::Style` that should be applied to a text segment
    /// belonging to the given element.
    ///
    /// `heading_level` is only meaningful when `element` is `Paragraph` but
    /// we are inside a heading; pass `Some(n)` in that case to get heading
    /// styling.
    #[must_use]
    pub fn style_for(&self, element: Option<MarkdownElement>, heading_level: Option<u8>) -> Style {
        let mut style = Style::default();

        // ── Heading (takes precedence over inline) ──────────────
        if let Some(level) = heading_level {
            let idx = (level.saturating_sub(1) as usize).min(5);
            let c = self.heading_colors[idx];
            style = style.fg(rgba_to_ratatui(c)).add_modifier(Modifier::BOLD);
        }

        // ── Inline element styling ──────────────────────────────
        match element {
            Some(MarkdownElement::Emphasis) => {
                style = style.add_modifier(Modifier::ITALIC);
            }
            Some(MarkdownElement::Strong) => {
                style = style.add_modifier(Modifier::BOLD);
            }
            Some(MarkdownElement::Strikethrough) => {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            Some(MarkdownElement::Link) => {
                style = style.fg(rgba_to_ratatui(self.link));
                if heading_level.is_none() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
            }
            Some(MarkdownElement::InlineCode) => {
                style = style
                    .fg(rgba_to_ratatui(self.inline_code_fg))
                    .bg(rgba_to_ratatui(self.inline_code_bg));
            }
            Some(MarkdownElement::CodeBlock) => {
                style = style.fg(rgba_to_ratatui(self.text));
            }
            Some(MarkdownElement::Blockquote) => {
                style = style.fg(rgba_to_ratatui(self.muted));
            }
            Some(
                MarkdownElement::Heading(_)
                | MarkdownElement::ListItem { .. }
                | MarkdownElement::Paragraph,
            )
            | None => {}
        }

        style
    }

    /// Convenience: ratatui `Color` for code-block background fill.
    #[must_use]
    pub const fn code_bg_color(&self) -> Color {
        rgba_to_ratatui(self.code_block_bg)
    }

    /// Convenience: ratatui `Color` for blockquote background fill.
    #[must_use]
    pub const fn quote_bg_color(&self) -> Color {
        rgba_to_ratatui(self.blockquote_bg)
    }

    /// Convenience: background fill for the entire area.
    #[must_use]
    pub const fn bg_color(&self) -> Color {
        rgba_to_ratatui(self.background)
    }
}

const fn rgba_to_ratatui(c: RGBA) -> Color {
    let (r, g, b, _) = c.to_ints();
    Color::Rgb(r, g, b)
}
