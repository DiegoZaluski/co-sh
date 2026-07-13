/// Unicode character/grapheme width utilities for terminal rendering.
///
/// Correctly handles:
/// - CJK characters (2 columns)
/// - Emoji (2 columns)
/// - Regional Indicator pairs (flag emojis 🇧🇷) - 2 columns per pair
/// - ZWJ sequences (rainbow flag 🏳🌈, pirate flag 🏴☠️) - 2 columns each
/// - Variation selectors (FE0F, FE0E) - zero width, part of parent grapheme
/// - Zero Width Joiner (U+200D) - zero width, part of parent grapheme
///
/// Uses `finl_unicode::grapheme_clusters::Graphemes` to split text into
/// grapheme clusters, then `unicode_width::UnicodeWidthStr::width` to
/// measure each cluster's display width (capped at 2 columns).

use finl_unicode::grapheme_clusters::Graphemes;

/// Returns the display width of a single grapheme in terminal columns.
///
/// A grapheme cluster can be:
/// - A single ASCII character: width 1
/// - A CJK ideograph: width 2
/// - An emoji: width 2
/// - A flag pair (two Regional Indicator chars): width 2
/// - A ZWJ sequence (e.g. 🏳🌈 = flag + ZWJ + rainbow): width 2
///
/// The width is always between 1 and 2 inclusive.
///
/// **Important:** Some characters (like U+2620 ☠ skull) are NOT classified
/// as wide by `unicode-width` even when followed by U+FE0F (Variation
/// Selector-16, which forces emoji presentation). In the terminal, those
/// characters occupy 2 columns when presented as emoji. We detect FE0F
/// explicitly to force width 2 for those cases.
#[must_use]
pub fn grapheme_display_width(g: &str) -> u16 {
    // If the grapheme contains U+FE0F (Variation Selector-16, forces emoji
    // presentation), the terminal renders it as a 2-column emoji.
    // This handles cases like ☠️ (skull + VS16) where unicode-width
    // returns width 1 but the terminal renders as 2 columns.
    if g.contains('\u{FE0F}') {
        return 2;
    }
    // If the grapheme contains U+FE0E (Variation Selector-15, forces text
    // presentation), cap at 1 column regardless of unicode-width.
    if g.contains('\u{FE0E}') {
        return 1;
    }
    let w = unicode_width::UnicodeWidthStr::width(g);
    if w == 0 {
        // Zero-width grapheme (e.g., standalone combining mark)
        // Use 1 to prevent infinite loops in rendering loops
        1
    } else {
        w.min(2) as u16
    }
}

/// Returns the display width of each individual character (for simple,
/// non-grapheme-aware use). Use `grapheme_display_width` for correct
/// results with complex emoji.
#[must_use]
#[inline]
pub fn char_display_width(c: char) -> u16 {
    match unicode_width::UnicodeWidthChar::width(c) {
        Some(0) => 0,
        Some(1) => 1,
        Some(2) => 2,
        _ => 1,
    }
}

/// Returns the display width of a string in terminal columns.
///
/// Iterates over grapheme clusters and sums their display widths
/// (capped at 2 per grapheme). This is the correct way to measure
/// how many terminal columns a string occupies.
#[must_use]
pub fn str_display_width(s: &str) -> usize {
    Graphemes::new(s)
        .map(|g| grapheme_display_width(&g) as usize)
        .sum()
}

/// Iterate over the grapheme clusters in a string, yielding each
/// grapheme along with its display width.
///
/// This is the primary rendering primitive: instead of iterating
/// `text.chars()`, iterate `graphemes_with_width(text)` to correctly
/// position each visual unit in the terminal grid.
pub fn graphemes_with_width<'a>(
    text: &'a str,
) -> impl Iterator<Item = (&'a str, u16)> + 'a {
    Graphemes::new(text).map(|g| {
        let w = grapheme_display_width(g);
        (g, w)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_1() {
        assert_eq!(grapheme_display_width("a"), 1);
        assert_eq!(grapheme_display_width("Z"), 1);
        assert_eq!(grapheme_display_width(" "), 1);
        assert_eq!(grapheme_display_width("!"), 1);
    }

    #[test]
    fn cjk_is_2() {
        assert_eq!(grapheme_display_width("中"), 2);
        assert_eq!(grapheme_display_width("文"), 2);
    }

    #[test]
    fn emoji_is_2() {
        assert_eq!(grapheme_display_width("😀"), 2);
        assert_eq!(grapheme_display_width("🤷"), 2);
    }

    #[test]
    fn flag_regional_indicator_pair() {
        // Flag of Brazil: 🇧 + 🇷 = single grapheme, width capped at 2
        let flag = "\u{1F1E7}\u{1F1F7}";
        assert_eq!(grapheme_display_width(flag), 2);
    }

    #[test]
    fn zwj_rainbow_flag() {
        // Rainbow flag: white flag + ZWJ + rainbow
        // Individual chars: 2 + 0 + 0 + 2 = 4, but capped at 2
        let rainbow_flag = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
        assert_eq!(grapheme_display_width(rainbow_flag), 2);
    }

    #[test]
    fn zwj_pirate_flag() {
        // Pirate flag: black flag + ZWJ + skull + VS16
        let pirate_flag = "\u{1F3F4}\u{200D}\u{2620}\u{FE0F}";
        assert_eq!(grapheme_display_width(pirate_flag), 2);
    }

    #[test]
    fn skull_vs16_is_2() {
        // ☠ (U+2620) alone: width 1 in unicode-width
        assert_eq!(grapheme_display_width("\u{2620}"), 1);
        // ☠️ (U+2620 + VS16): width 2 because of FE0F
        assert_eq!(grapheme_display_width("\u{2620}\u{FE0F}"), 2);
    }

    #[test]
    fn text_variation_selector_15() {
        // ✊ (U+270A, raised fist): inherently emoji, width 2
        assert_eq!(grapheme_display_width("\u{270A}"), 2);
        // ✊︎ (U+270A + FE0E): forced text presentation, width 1
        assert_eq!(grapheme_display_width("\u{270A}\u{FE0E}"), 1);
    }

    #[test]
    fn str_width_simple() {
        assert_eq!(str_display_width("abc"), 3);
        assert_eq!(str_display_width("中文字"), 6);
        assert_eq!(str_display_width("a中b"), 4);
    }

    #[test]
    fn str_width_with_flags() {
        // Flag of Brazil pair: 1 grapheme, width 2
        assert_eq!(str_display_width("\u{1F1E7}\u{1F1F7}"), 2);
        // Multiple flags: each flag pair is width 2
        assert_eq!(str_display_width("\u{1F1E7}\u{1F1F7}\u{1F1FA}\u{1F1F8}"), 4);
    }

    #[test]
    fn str_width_with_zwj() {
        // Rainbow flag: 1 grapheme, width 2
        assert_eq!(str_display_width("\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}"), 2);
    }
}
