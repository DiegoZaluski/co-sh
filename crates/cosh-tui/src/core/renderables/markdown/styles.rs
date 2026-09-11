use ratatui::style::{Color, Modifier, Style};

use crate::core::rgba::RGBA;
use cosh_sdk::tree_sitter::highlight::HighlightCategory;

use super::context::MarkdownElement;

/// Explicit per-element colors a host theme can supply. Every field is
/// optional: `None` keeps the value derived from the base fg/bg, so partial
/// themes degrade gracefully.
#[derive(Debug, Clone, Default)]
pub struct MarkdownAccentColors {
    /// Base for the H1→H6 ramp (H1 uses it directly; lower levels blend
    /// toward muted).
    pub heading: Option<RGBA>,
    /// Link color: URL/decoration part and unconcealed `[label](url)`.
    pub link: Option<RGBA>,
    /// Concealed-mode link label color (the visible text of a link).
    pub link_label: Option<RGBA>,
    /// Inline `` `code` `` foreground.
    pub inline_code_fg: Option<RGBA>,
    /// Blockquote vertical bar.
    pub blockquote_bar: Option<RGBA>,
    /// `*emphasis*` foreground (rendered italic).
    pub emph: Option<RGBA>,
    /// `**strong**` foreground (rendered bold).
    pub strong: Option<RGBA>,
    /// `---` horizontal rule.
    pub horizontal_rule: Option<RGBA>,
    /// Bullets (`•`) and task-list checkboxes.
    pub list_marker: Option<RGBA>,
    /// Ordered-list numbers (`1.`).
    pub list_enumeration: Option<RGBA>,
    /// Table header row foreground (rendered bold).
    pub table_header: Option<RGBA>,
}

/// Syntax-highlighting colors for fenced code blocks, mapped onto
/// tree-sitter highlight categories. Unset categories keep the built-in
/// fallback colors.
#[derive(Debug, Clone, Default)]
pub struct SyntaxColors {
    pub comment: Option<RGBA>,
    pub keyword: Option<RGBA>,
    pub function: Option<RGBA>,
    pub string: Option<RGBA>,
    pub number: Option<RGBA>,
    pub r#type: Option<RGBA>,
    pub builtin: Option<RGBA>,
}

/// Theme-aware palette that maps semantic `MarkdownElement`s to concrete
/// `ratatui::Style` values.
///
/// Conceptually similar to a syntax style's per-group lookups:
/// each markdown construct (heading, emphasis, link, etc.) gets a
/// distinct visual treatment derived from the base text/background colors,
/// overridden per element by anything supplied through
/// [`MarkdownPalette::set_accent_colors`] / [`MarkdownPalette::set_syntax_colors`].
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

    // ── Theme overrides (empty = fully derived from fg/bg) ─────
    accents: MarkdownAccentColors,
    syntax: SyntaxColors,

    // ── Warning quote theme (configurable) ─────────────────────
    /// A quote whose body starts with this marker renders with the warning
    /// style below. Empty prefix disables the feature.
    warning_prefix: String,
    warning_fg: RGBA,
}

impl MarkdownPalette {
    /// Construct a palette derived from the given foreground (text) and
    /// background colours.
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

        // Blockquote bg = transparent (no background fill — indented text with muted color is sufficient)
        let blockquote_bg = RGBA::from_ints(0, 0, 0, 0);

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
            accents: MarkdownAccentColors::default(),
            syntax: SyntaxColors::default(),
            warning_prefix: "\u{26A0}".to_string(), // ⚠
            warning_fg: RGBA::from_ints(238, 241, 112, 255),
        }
    }

    // ── Theme overrides ────────────────────────────────────────

    /// Apply host-theme accent colors. Supplied values override the
    /// derived defaults element by element.
    pub fn set_accent_colors(&mut self, accents: MarkdownAccentColors) {
        if let Some(heading) = accents.heading {
            // Rebuild the H1→H6 ramp from the theme color: H1 keeps it in
            // full, H2–H5 step down toward muted, H6 lands on muted — the
            // same shape the derivation uses for plain text colors.
            let (hr, hg, hb, _) = heading.to_ints();
            let (mr, mg, mb, _) = self.muted.to_ints();
            let mix = |t: u8, m: u8, num: u16| -> u8 {
                ((u16::from(t) * num + u16::from(m) * (12 - num)) / 12).min(255) as u8
            };
            let ramp = [
                heading,
                RGBA::from_ints(mix(hr, mr, 11), mix(hg, mg, 11), mix(hb, mb, 11), 255),
                RGBA::from_ints(mix(hr, mr, 10), mix(hg, mg, 10), mix(hb, mb, 10), 255),
                RGBA::from_ints(mix(hr, mr, 9), mix(hg, mg, 9), mix(hb, mb, 9), 255),
                RGBA::from_ints(mix(hr, mr, 8), mix(hg, mg, 8), mix(hb, mb, 8), 255),
                self.muted,
            ];
            self.heading_colors = ramp;
        }
        if let Some(link) = accents.link {
            self.link = link;
        }
        if let Some(bar) = accents.blockquote_bar {
            self.blockquote_bar = bar;
        }
        if let Some(marker) = accents.list_marker {
            self.list_marker = marker;
        }
        self.accents = accents;
    }

    /// Apply host-theme syntax-highlighting colors for code blocks.
    pub fn set_syntax_colors(&mut self, syntax: SyntaxColors) {
        self.syntax = syntax;
    }

    #[must_use]
    pub const fn accent_colors(&self) -> &MarkdownAccentColors {
        &self.accents
    }

    #[must_use]
    pub const fn syntax_colors(&self) -> &SyntaxColors {
        &self.syntax
    }

    // ── Warning theme ──────────────────────────────────────────

    /// Customize the warning-quote theme: a quote whose body starts with
    /// `prefix` renders its text in `fg` instead of the defaults (⚠ /
    /// yellow text, no background). An empty prefix disables warning
    /// styling entirely.
    pub fn set_warning_theme(&mut self, prefix: impl Into<String>, fg: RGBA) {
        self.warning_prefix = prefix.into();
        self.warning_fg = fg;
    }

    #[must_use]
    pub fn warning_prefix(&self) -> &str {
        &self.warning_prefix
    }

    #[must_use]
    pub const fn warning_fg_color(&self) -> RGBA {
        self.warning_fg
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

    /// Ordered-list number color (falls back to the bullet marker color).
    #[must_use]
    pub fn list_enumeration_color(&self) -> RGBA {
        self.accents.list_enumeration.unwrap_or(self.list_marker)
    }

    /// Horizontal-rule color (falls back to muted).
    #[must_use]
    pub fn horizontal_rule_color(&self) -> RGBA {
        self.accents.horizontal_rule.unwrap_or(self.muted)
    }

    /// Table header row color (falls back to text).
    #[must_use]
    pub fn table_header_color(&self) -> RGBA {
        self.accents.table_header.unwrap_or(self.text)
    }

    /// Concealed-mode link label style: label color when the theme supplies
    /// one, otherwise the regular link treatment.
    #[must_use]
    pub fn link_label_style(&self, in_heading: bool) -> Style {
        match self.accents.link_label {
            Some(c) => {
                let mut style = Style::default().fg(rgba_to_ratatui(c));
                if !in_heading {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                style
            }
            None => self.style_for(Some(MarkdownElement::Link), None),
        }
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
                if let Some(c) = self.accents.emph {
                    style = style.fg(rgba_to_ratatui(c));
                }
            }
            Some(MarkdownElement::Strong) => {
                style = style.add_modifier(Modifier::BOLD);
                if let Some(c) = self.accents.strong {
                    style = style.fg(rgba_to_ratatui(c));
                }
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
                let mut fg = self.inline_code_fg;
                if let Some(c) = self.accents.inline_code_fg {
                    fg = c;
                }
                style = style
                    .fg(rgba_to_ratatui(fg))
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
            | None => {
                // Plain text (paragraphs, bare runs, list item bodies) must
                // keep the palette's base text color. Without this the cells
                // keep the default `Reset` fg, so the terminal's own default
                // foreground leaks through — invisible on light themes (e.g.
                // sakura) where the default is white on a light background.
                // Headings/inline accents already set a fg above, so only the
                // untouched style gets the base color.
                if style.fg.is_none() {
                    style = style.fg(rgba_to_ratatui(self.text));
                }
            }
        }

        style
    }

    /// Convenience: ratatui `Color` for code-block background fill.
    #[must_use]
    pub const fn code_bg_color(&self) -> Color {
        rgba_to_ratatui(self.code_block_bg)
    }

    /// Resolve a tree-sitter highlight category to the palette's syntax
    /// color for it. `None` categories and unset slots return `None` so the
    /// caller can fall back to its own defaults.
    #[must_use]
    pub fn syntax_color(&self, cat: HighlightCategory) -> Option<RGBA> {
        let s = &self.syntax;
        match cat {
            HighlightCategory::Comment => s.comment,
            HighlightCategory::Keyword => s.keyword,
            HighlightCategory::Function => s.function,
            HighlightCategory::String => s.string,
            HighlightCategory::Number => s.number,
            HighlightCategory::Type => s.r#type,
            HighlightCategory::Builtin => s.builtin,
        }
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

pub const fn rgba_to_ratatui(c: RGBA) -> Color {
    let (r, g, b, _) = c.to_ints();
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REGRESSION: plain paragraph/bare text must keep the palette's base
    /// `text` foreground. Before the fix, `style_for` returned `Style::default()`
    /// (no fg) for paragraphs, so the terminal's own default foreground leaked
    /// through — white on light themes like sakura made the output unreadable.
    #[test]
    fn paragraph_text_uses_base_text_fg() {
        let text = RGBA::from_ints(0, 0, 0, 255);
        let palette = MarkdownPalette::new(text, RGBA::from_ints(232, 240, 255, 255));

        for element in [
            None,
            Some(MarkdownElement::Paragraph),
            Some(MarkdownElement::ListItem { ordered: true }),
        ] {
            let style = palette.style_for(element, None);
            assert_eq!(
                style.fg,
                Some(rgba_to_ratatui(text)),
                "plain text must carry the base fg for element {element:?}"
            );
        }
    }

    /// Inline accents still win over the new base-text fallback.
    #[test]
    fn inline_accents_override_base_fg() {
        let text = RGBA::from_ints(0, 0, 0, 255);
        let mut palette = MarkdownPalette::new(text, RGBA::from_ints(232, 240, 255, 255));
        let strong = RGBA::from_ints(200, 100, 50, 255);
        palette.set_accent_colors(MarkdownAccentColors {
            strong: Some(strong),
            ..MarkdownAccentColors::default()
        });
        let style = palette.style_for(Some(MarkdownElement::Strong), None);
        assert_eq!(style.fg, Some(rgba_to_ratatui(strong)));
    }
}
