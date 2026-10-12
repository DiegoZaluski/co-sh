//! The context-budget readout: one bar component, every consumer.
//!
//! Normal mode paints `9,612 / 100,000 tok` plus `▓▓▓░░░░░░░ 45%`
//! top-right of the session header; the `/agents` pane headers paint the
//! same pair (from the pane's ACP `UsageUpdate` snapshot) top-right of
//! each pane; the weights-install chat line mirrors the bar. The text
//! composition, the percentage math, the gap and the bar colors live here
//! so the consumers can never drift apart.

use ratatui::style::{Modifier, Style};

use crate::theme::{Theme, rgba_color};

/// Cells between the `used / size tok` counter and the bar: the session
/// header's gap, reused by the pane headers verbatim.
pub(crate) const BUDGET_GAP: u16 = 2;

/// Render a 10-character budget bar like `▓▓▓▓▓░░░░░` from a 0-100 percentage.
pub(crate) fn render_budget_bar(pct: u8) -> String {
    const FILLED: char = '▓';
    const EMPTY: char = '░';
    const BAR_LEN: usize = 10;
    let filled = (pct as usize * BAR_LEN) / 100;
    let empty = BAR_LEN.saturating_sub(filled);
    std::iter::repeat_n(FILLED, filled)
        .chain(std::iter::repeat_n(EMPTY, empty))
        .collect()
}

/// Format an integer with thousands separators (e.g. `9612` → `9,612`) so
/// large token counts stay readable at a glance in the header bar.
pub(crate) fn format_tokens(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Percentage of `size` already `used`: the harness `display_info` math
/// (float ratio capped at 100). A zero window reads 0 — never divide by
/// zero, never guess a window.
pub(crate) fn usage_pct(used: u64, size: u64) -> u8 {
    if size > 0 {
        ((used as f64 / size as f64) * 100.0).min(100.0) as u8
    } else {
        0
    }
}

/// The header's `(token_str, budget_str, pct)` triple: `9,612 / 100,000
/// tok` (bare count when the window is unknown) plus the bar with its
/// right-aligned percentage.
pub(crate) fn budget_texts(used: u64, size: u64) -> (String, String, u8) {
    let pct = usage_pct(used, size);
    let token_str = if size > 0 {
        format!("{} / {} tok", format_tokens(used), format_tokens(size))
    } else {
        format!("{} tok", format_tokens(used))
    };
    let budget_str = format!("{}{:>3}%", render_budget_bar(pct), pct);
    (token_str, budget_str, pct)
}

/// Short token counts for the compact variant: `9612` → `9.6k`,
/// `100_000` → `100k`, `1_200_000` → `1.2M`. One decimal, no trailing
/// `.0`, so the pair stays readable in a narrow pane.
pub(crate) fn format_tokens_short(n: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000 * K;
    const B: u64 = 1_000 * M;
    fn scaled(n: u64, unit: u64, suffix: &str) -> String {
        let whole = n / unit;
        let frac = (n % unit) / (unit / 10);
        if frac == 0 {
            format!("{whole}{suffix}")
        } else {
            format!("{whole}.{frac}{suffix}")
        }
    }
    if n < K {
        n.to_string()
    } else if n < M {
        scaled(n, K, "k")
    } else if n < B {
        scaled(n, M, "M")
    } else {
        scaled(n, B, "B")
    }
}

/// One painted budget block in either width variant: the `used` counter,
/// the usage meter (the bar; empty when compact) and the percentage text.
/// Both consumers paint `counter` muted, `meter` in [`budget_bar_style`]
/// and `pct_text` in [`budget_pct_style`] (same tone, bold); only the
/// variant (and therefore the width) differs.
pub(crate) struct BudgetBlock {
    pub counter: String,
    pub meter: String,
    pub pct_text: String,
    pub gap: u16,
    pub pct: u8,
}

impl BudgetBlock {
    /// Cell width of the painted block: counter + gap + meter + percent.
    pub(crate) fn width(&self) -> u16 {
        (self.counter.chars().count() + self.meter.chars().count() + self.pct_text.chars().count())
            as u16
            + self.gap
    }

    /// The full pair, unconditionally: `9,612 / 100,000 tok` + the bar
    /// with its percentage.
    pub(crate) fn full(used: u64, size: u64) -> Self {
        let (counter, _, pct) = budget_texts(used, size);
        Self {
            counter,
            meter: render_budget_bar(pct),
            pct_text: format!("{:>3}%", pct),
            gap: BUDGET_GAP,
            pct,
        }
    }

    /// The compact pair: `9.6k/100k 9%` — the same three numbers (used,
    /// budget, percent), no bar, no `tok`, single-space gap.
    fn compact(used: u64, size: u64) -> Self {
        let pct = usage_pct(used, size);
        let counter = if size > 0 {
            format!(
                "{}/{}",
                format_tokens_short(used),
                format_tokens_short(size)
            )
        } else {
            format_tokens_short(used)
        };
        Self {
            counter,
            meter: String::new(),
            pct_text: format!("{pct}%"),
            gap: 1,
            pct,
        }
    }
}

/// Pick the widest variant that fits `avail_w` COLUMNS (horizontal space
/// only): the full pair, else the compact one, else `None` — the caller
/// then paints no budget (the header hides its price the same way when
/// nothing is known).
pub(crate) fn budget_block(used: u64, size: u64, avail_w: u16) -> Option<BudgetBlock> {
    let full = BudgetBlock::full(used, size);
    if full.width() <= avail_w {
        return Some(full);
    }
    let compact = BudgetBlock::compact(used, size);
    (compact.width() <= avail_w).then_some(compact)
}
pub(crate) fn budget_bar_style(pct: u8, theme: &Theme) -> Style {
    if pct >= 90 {
        Style::default().fg(rgba_color(theme.error))
    } else if pct >= 70 {
        Style::default().fg(rgba_color(theme.warning))
    } else {
        Style::default().fg(rgba_color(theme.text_muted))
    }
}

/// The budget percentage's style: the bar's conditional tone, bold, so
/// the number reads first at a glance.
pub(crate) fn budget_pct_style(pct: u8, theme: &Theme) -> Style {
    budget_bar_style(pct, theme).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    #[test]
    fn format_tokens_small_values_have_no_separator() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(9), "9");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_groups_thousands() {
        assert_eq!(format_tokens(1_000), "1,000");
        assert_eq!(format_tokens(9_612), "9,612");
        assert_eq!(format_tokens(100_000), "100,000");
        assert_eq!(format_tokens(1_000_000), "1,000,000");
        assert_eq!(format_tokens(1_234_567), "1,234,567");
        assert_eq!(format_tokens(12_345_678), "12,345,678");
    }

    #[test]
    fn format_tokens_handles_large_values() {
        assert_eq!(format_tokens(123_456), "123,456");
        assert_eq!(format_tokens(9_876_543_210), "9,876,543,210");
        assert_eq!(format_tokens(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn usage_pct_zero_window_reads_zero() {
        assert_eq!(usage_pct(9_612, 0), 0);
    }

    #[test]
    fn usage_pct_caps_past_full() {
        assert_eq!(usage_pct(50_000, 100_000), 50);
        assert_eq!(usage_pct(100_000, 100_000), 100);
        assert_eq!(usage_pct(200_000, 100_000), 100);
    }

    #[test]
    fn budget_texts_pairs_counter_with_bar() {
        let (token_str, budget_str, pct) = budget_texts(9_612, 100_000);
        assert_eq!(token_str, "9,612 / 100,000 tok");
        assert_eq!(pct, 9);
        assert_eq!(budget_str, format!("{}  9%", render_budget_bar(9)));
    }

    #[test]
    fn format_tokens_short_scales_with_one_decimal() {
        assert_eq!(format_tokens_short(0), "0");
        assert_eq!(format_tokens_short(999), "999");
        assert_eq!(format_tokens_short(1_000), "1k");
        assert_eq!(format_tokens_short(9_612), "9.6k");
        assert_eq!(format_tokens_short(10_000), "10k");
        assert_eq!(format_tokens_short(100_000), "100k");
        assert_eq!(format_tokens_short(1_200_000), "1.2M");
        assert_eq!(format_tokens_short(25_000_000_000), "25B");
    }

    #[test]
    fn budget_block_picks_full_when_it_fits() {
        let block = budget_block(9_612, 100_000, 60).expect("fits");
        assert_eq!(block.counter, "9,612 / 100,000 tok");
        assert_eq!(block.meter, render_budget_bar(9));
        assert_eq!(block.pct_text, "  9%");
        assert_eq!(block.gap, BUDGET_GAP);
        assert_eq!(block.pct, 9);
        assert_eq!(block.width(), 35);
    }

    #[test]
    fn budget_block_drops_to_compact_on_narrow_width() {
        let block = budget_block(9_612, 100_000, 20).expect("compact fits");
        assert_eq!(block.counter, "9.6k/100k");
        assert!(block.meter.is_empty(), "compact carries no bar");
        assert_eq!(block.pct_text, "9%");
        assert_eq!(block.gap, 1);
        assert_eq!(block.pct, 9);
        assert_eq!(block.width(), 12);
    }

    #[test]
    fn budget_block_gives_up_when_not_even_compact_fits() {
        assert!(budget_block(9_612, 100_000, 11).is_none());
        // Exact fit still paints.
        assert!(budget_block(9_612, 100_000, 12).is_some());
    }

    #[test]
    fn budget_texts_unknown_window_shows_bare_count() {
        let (token_str, _, pct) = budget_texts(9_612, 0);
        assert_eq!(token_str, "9,612 tok");
        assert_eq!(pct, 0);
    }

    #[test]
    fn budget_pct_style_bolds_the_bar_tone() {
        let theme = ThemeRegistry::new().default_theme().clone();
        let style = budget_pct_style(9, &theme);
        assert_eq!(style.fg, rgba_color(theme.text_muted).into());
        assert!(
            style.add_modifier.contains(Modifier::BOLD),
            "the percentage reads bold"
        );
        assert!(
            !budget_bar_style(9, &theme)
                .add_modifier
                .contains(Modifier::BOLD),
            "the bar itself stays plain"
        );
    }

    #[test]
    fn budget_bar_style_follows_the_header_thresholds() {
        let theme = ThemeRegistry::new().default_theme().clone();
        assert_eq!(
            budget_bar_style(9, &theme).fg,
            rgba_color(theme.text_muted).into()
        );
        assert_eq!(
            budget_bar_style(70, &theme).fg,
            rgba_color(theme.warning).into()
        );
        assert_eq!(
            budget_bar_style(90, &theme).fg,
            rgba_color(theme.error).into()
        );
    }
}
