# `unicode_util` — display widths and wrapping

Terminal cells are a fixed-width grid, but Unicode text isn't: CJK
ideographs take 2 columns, emoji take 2, flag pairs and ZWJ sequences
collapse to one grapheme of width 2, and variation selectors are zero-width.
`unicode_util` measures text in *terminal columns* correctly and wraps text
to a width, so renderables don't corrupt layout with wide or combining
characters.

The module uses `finl_unicode::grapheme_clusters::Graphemes` to split text
into grapheme clusters and `unicode-width` to measure each cluster, with
explicit handling for emoji presentation.

## Measuring widths

```rust
pub fn grapheme_display_width(g: &str) -> u16      // 1 or 2 columns
pub fn char_display_width(c: char) -> u16          // simple per-char width (0/1/2)
pub fn str_display_width(s: &str) -> usize         // sum of grapheme widths
pub fn graphemes_with_width(text: &str) -> impl Iterator<Item = (&str, u16)> + '_
```

The width rules, per grapheme:

| Grapheme | Width |
|---|---|
| ASCII, most Latin | 1 |
| CJK ideographs | 2 |
| Emoji | 2 |
| Flag pair (two Regional Indicators, 🇧🇷) | 2 |
| ZWJ sequence (🏳️‍🌈) | 2 (the whole sequence is one grapheme) |
| Grapheme containing U+FE0F (VS-16, emoji presentation) | forced 2 |
| Grapheme containing U+FE0E (VS-15, text presentation) | forced 1 |
| Zero-width (standalone combining mark) | forced 1 (avoids render-loop stalls) |

`graphemes_with_width` is the **primary rendering primitive**: instead of
iterating `text.chars()`, renderables iterate this to position each visual
unit, marking `CellDiffOption::Skip` on the extra columns of a wide grapheme.

## Wrapping

```rust
pub fn word_wrap(text: &str, max_width: u16) -> Vec<String>
```

Wraps text to fit `max_width` columns, keeping words intact where possible.
Words longer than the whole line are broken at character level so no line
exceeds the width. Newlines are respected.

```rust,ignore
assert_eq!(word_wrap("a b c", 3), vec!["a b".to_string(), "c".to_string()]);
assert_eq!(word_wrap("中文字", 4), vec!["中文".to_string(), "字".to_string()]); // 2-col chars
```

---

## Example — measuring mixed text

```rust,ignore
use cosh_tui::core::lib::unicode_util::{str_display_width, grapheme_display_width, word_wrap};

assert_eq!(str_display_width("abc"), 3);
assert_eq!(str_display_width("中文字"), 6);                 // three 2-col chars
assert_eq!(str_display_width("a中b"), 4);
assert_eq!(str_display_width("🇧🇷"), 2);                    // flag pair = one grapheme
assert_eq!(grapheme_display_width("☠\u{fe0f}"), 2);        // skull + VS-16 → emoji
assert_eq!(grapheme_display_width("✊\u{fe0e}"), 1);        // fist + VS-15 → text

for (g, w) in graphemes_with_width("a中") {
    // ("a", 1), ("中", 2)
}
```

Next: [detect_links — URL detection](detect_links.md).
