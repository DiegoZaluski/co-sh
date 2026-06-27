//! Unicode character properties
//! Stripped-down inline replacement for wezterm-char-props
//! Provides emoji presentation, widechar width, and white space detection

/// Emoji vs text presentation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presentation {
    Text,
    Emoji,
}

impl Presentation {
    /// Determine presentation for a grapheme.
    /// Returns (default_presentation, optional_variation_override).
    /// A VariationSelector-16 (U+FE0F) forces Emoji, VS-15 (U+FE0E) forces Text.
    pub fn for_grapheme(s: &str) -> (Presentation, Option<Presentation>) {
        let mut chars = s.chars();
        let base = chars.next();
        let rest: Vec<char> = chars.collect();

        if rest.contains(&'\u{FE0F}') {
            // Explicit emoji variation selector
            let base_emoji = is_emoji_presentation(base.unwrap_or(' '));
            if base_emoji {
                (Presentation::Emoji, Some(Presentation::Emoji))
            } else {
                (Presentation::Text, Some(Presentation::Emoji))
            }
        } else if rest.contains(&'\u{FE0E}') {
            // Text variation selector: only meaningful for characters
            // with Emoji_Presentation=No. For characters that are
            // inherently emoji (Emoji_Presentation=Yes), FE0E is
            // not valid and is ignored.
            let base_emoji = is_emoji_presentation(base.unwrap_or(' '));
            if base_emoji {
                (Presentation::Emoji, None)
            } else {
                (Presentation::Text, Some(Presentation::Text))
            }
        } else if let Some(c) = base {
            if is_emoji_presentation(c) {
                (Presentation::Emoji, None)
            } else {
                (Presentation::Text, None)
            }
        } else {
            (Presentation::Text, None)
        }
    }
}

/// Character width classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WcWidth {
    Zero,
    One,
    Two,
    Ambiguous,
    Unassigned,
}

impl WcWidth {
    pub fn width_unicode_9_or_later(self) -> u32 {
        match self {
            WcWidth::Two | WcWidth::Ambiguous => 2,
            WcWidth::One => 1,
            WcWidth::Zero => 0,
            WcWidth::Unassigned => 1,
        }
    }

    pub fn width_unicode_8_or_earlier(self) -> u32 {
        match self {
            WcWidth::Two => 2,
            WcWidth::Ambiguous => 1,
            WcWidth::One => 1,
            WcWidth::Zero => 0,
            WcWidth::Unassigned => 1,
        }
    }
}

/// A simplified width table: classify a char using unicode-width
pub fn wcwidth_classify(c: char) -> WcWidth {
    // Special cases: line/paragraph separator have zero width
    if matches!(c, '\u{2028}' | '\u{2029}') {
        return WcWidth::Zero;
    }
    let w = unicode_width::UnicodeWidthChar::width(c);
    match w {
        Some(0) => WcWidth::Zero,
        Some(1) => WcWidth::One,
        Some(2) => {
            if c > '\u{FFFF}' {
                WcWidth::Two
            } else {
                WcWidth::Ambiguous
            }
        }
        _ => {
            if (c >= '\u{3000}' && c <= '\u{3000}')
                || (c >= '\u{FF01}' && c <= '\u{FF60}')
                || (c >= '\u{FFE0}' && c <= '\u{FFE6}')
            {
                WcWidth::Two
            } else if c > '\u{00FF}' {
                WcWidth::Ambiguous
            } else {
                WcWidth::One
            }
        }
    }
}

/// White space property table
pub struct WhiteSpaceTable;

impl WhiteSpaceTable {
    pub fn contains_u32(&self, c: u32) -> bool {
        char::from_u32(c).map_or(false, |ch| ch.is_whitespace())
    }
}

pub static WHITE_SPACE: WhiteSpaceTable = WhiteSpaceTable;

/// Inline replacement for wezterm_char_props::emoji_variation::WCWIDTH_TABLE
pub mod emoji_variation {
    use super::WcWidth;
    pub struct WcWidthTable;
    impl WcWidthTable {
        pub fn classify(&self, c: char) -> WcWidth {
            super::wcwidth_classify(c)
        }
    }
    pub static WCWIDTH_TABLE: WcWidthTable = WcWidthTable;
}

/// Inline replacement for wezterm_char_props::emoji
pub mod emoji {
    pub use super::Presentation;
}

/// Inline replacement for wezterm_char_props::widechar_width
pub mod widechar_width {
    pub use super::WcWidth;
}

/// Inline replacement for wezterm_char_props::white_space
pub mod white_space {
    pub use super::WHITE_SPACE;
}

fn is_emoji_presentation(c: char) -> bool {
    use unicode_width::UnicodeWidthChar;
    UnicodeWidthChar::width(c) == Some(2)
        || matches!(c,
            '\u{231A}' | '\u{231B}' | '\u{23E9}'..='\u{23F3}' |
            '\u{23F8}'..='\u{23FA}' | '\u{25FD}' | '\u{25FE}' |
            '\u{2614}' | '\u{2615}' | '\u{2648}'..='\u{2653}' |
            '\u{267F}' | '\u{2693}' | '\u{26A1}' | '\u{26AA}' | '\u{26AB}' |
            '\u{26BD}' | '\u{26BE}' | '\u{26C4}' | '\u{26C5}' |
            '\u{26CE}' | '\u{26D4}' | '\u{26EA}' | '\u{26F2}'..='\u{26F5}' |
            '\u{26F9}'..='\u{26FA}' | '\u{26FD}' |
            '\u{2702}' | '\u{2705}' | '\u{2708}' | '\u{270A}' | '\u{270B}' | '\u{270D}' |
            '\u{270F}' | '\u{2712}' | '\u{2714}' | '\u{2716}' | '\u{271D}' |
            '\u{2721}' | '\u{2728}' | '\u{2733}' | '\u{2734}' | '\u{2744}' |
            '\u{2747}' | '\u{274C}' | '\u{274E}' | '\u{2753}'..='\u{2755}' |
            '\u{2757}' | '\u{2763}' | '\u{2764}' | '\u{2795}'..='\u{2797}' |
            '\u{27A1}' | '\u{27B0}' | '\u{27BF}' |
            '\u{2934}' | '\u{2935}' |
            '\u{2B05}'..='\u{2B07}' | '\u{2B1B}' | '\u{2B1C}' | '\u{2B50}' | '\u{2B55}' |
            '\u{3030}' | '\u{303D}' | '\u{3297}' | '\u{3299}' |
            '\u{1F000}'..='\u{1FFFF}'
        )
}
