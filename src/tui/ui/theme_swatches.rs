use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

use crate::theme::{Theme, rgba_color};

/// Terminal cells each swatch chip occupies. Two cells read as a small
/// square-ish block on the typical 1:2 cell aspect ratio; one cell would
/// render as a thin bar.
const SWATCH_WIDTH: u16 = 2;

/// Minimum per-channel difference for two palette colors to count as
/// distinct chips. Themes often alias `accent` onto `secondary`; without
/// this filter the strip would show repeated identical blocks.
const DISTINCT_CHANNEL_DELTA: u8 = 16;

/// Full-block glyph used to paint a chip cell.
const SWATCH_GLYPH: char = '\u{2588}';

/// Drop candidates that are near-identical to an already-kept color,
/// preserving the candidate order so the strip stays stable across renders.
pub fn dedupe_swatches(candidates: &[RGBA]) -> Vec<RGBA> {
    let mut kept: Vec<RGBA> = Vec::with_capacity(candidates.len());
    for &candidate in candidates {
        let distinct = kept.iter().all(|kept_color| {
            let (kr, kg, kb, _) = kept_color.to_ints();
            let (cr, cg, cb, _) = candidate.to_ints();
            kr.abs_diff(cr) >= DISTINCT_CHANNEL_DELTA
                || kg.abs_diff(cg) >= DISTINCT_CHANNEL_DELTA
                || kb.abs_diff(cb) >= DISTINCT_CHANNEL_DELTA
        });
        if distinct {
            kept.push(candidate);
        }
    }
    kept
}

/// The palette fingerprint of a theme: its most identity-carrying slots
/// (primary, secondary, accent, success) with near-duplicates removed.
/// Pure and deterministic, so the same theme always renders the same strip.
pub fn theme_swatches(theme: &Theme) -> Vec<RGBA> {
    dedupe_swatches(&[theme.primary, theme.secondary, theme.accent, theme.success])
}

/// Total width in cells of a strip of `count` chips.
pub const fn swatches_width(count: usize) -> u16 {
    count as u16 * SWATCH_WIDTH
}

/// Draw a horizontal strip of color chips: `SWATCH_WIDTH` contiguous
/// full-block cells per color, foreground = the chip color, background =
/// the row background `bg` behind it (so chips sit cleanly on both the
/// resting and the highlighted row fill).
pub fn draw_theme_swatches(buf: &mut Buffer, x: u16, y: u16, colors: &[RGBA], bg: Color) {
    let mut cx = x;
    for &color in colors {
        for _ in 0..SWATCH_WIDTH {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(SWATCH_GLYPH);
                cell.set_style(Style::default().fg(rgba_color(color)).bg(bg));
            }
            cx = cx.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn rgba(r: u8, g: u8, b: u8) -> RGBA {
        RGBA::from_ints(r, g, b, 255)
    }

    #[test]
    fn near_identical_candidates_collapse_into_one_chip() {
        let kept = dedupe_swatches(&[rgba(10, 10, 10), rgba(20, 20, 20), rgba(200, 10, 10)]);
        assert_eq!(kept, vec![rgba(10, 10, 10), rgba(200, 10, 10)]);
    }

    #[test]
    fn distinct_candidates_are_all_kept_in_order() {
        let kept = dedupe_swatches(&[
            rgba(255, 0, 0),
            rgba(0, 255, 0),
            rgba(0, 0, 255),
            rgba(255, 0, 0),
        ]);
        assert_eq!(
            kept,
            vec![rgba(255, 0, 0), rgba(0, 255, 0), rgba(0, 0, 255)]
        );
    }

    #[test]
    fn every_registry_theme_yields_a_stable_nonempty_strip() {
        let registry = ThemeRegistry::new();
        for def in &registry.themes {
            let swatches = theme_swatches(&def.theme);
            assert!(!swatches.is_empty(), "{} produced no swatches", def.name);
            assert!(swatches.len() <= 4, "{} produced too many", def.name);
            // Deterministic: the same theme always renders the same strip.
            assert_eq!(swatches, theme_swatches(&def.theme));
            // Every chip is opaque so it paints as a solid block.
            for swatch in &swatches {
                assert_eq!(swatch.to_ints().3, 255);
            }
        }
    }

    #[test]
    fn strip_width_is_two_cells_per_chip() {
        assert_eq!(swatches_width(0), 0);
        assert_eq!(swatches_width(4), 8);
    }
}
