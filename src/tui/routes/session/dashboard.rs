//! The usage dashboard rendered in the left panel (opened with Ctrl+U).
//!
//! Shows, at a glance: the current session's real token/cost, then spend per
//! provider and the grand total for the selected period (day/week/month/year/
//! all), cycled with Tab / Shift+Tab.

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme::{Theme, rgba_color};
use crate::usage::SpendSummary;

/// A fully-computed snapshot the dashboard renders. Assembled by the App so
/// this module stays a pure view (easy to test).
#[derive(Debug, Clone)]
pub struct DashboardData {
    /// Tokens used by the current session (real API usage).
    pub session_tokens: u64,
    /// REAL cost of the current session as reported by the provider
    /// (`None` when the provider does not report costs — no price row,
    /// never an estimate).
    pub session_cost: Option<f64>,
    /// Spend for the selected period (per provider + total).
    pub period: SpendSummary,
    /// The period currently selected.
    pub period_enum: crate::usage::UsagePeriod,
}

/// Format a USD figure with two decimal places: `$0.00`, `$1.50`.
fn format_usd(v: f64) -> String {
    format!("${v:.2}")
}

/// Pick a highlight (background) color for a period so the active range is
/// instantly distinguishable — each period gets its own accent. Returns the
/// `RGBA` so the render can also derive a contrasting foreground from it.
fn period_highlight(
    period: crate::usage::UsagePeriod,
    theme: &Theme,
) -> cosh_tui::core::lib::rgba::RGBA {
    match period {
        crate::usage::UsagePeriod::Day => theme.success,
        crate::usage::UsagePeriod::Week => theme.primary,
        crate::usage::UsagePeriod::Month => theme.accent,
        crate::usage::UsagePeriod::Year => theme.info,
        crate::usage::UsagePeriod::All => theme.warning,
    }
}

/// Format tokens compactly with thousands separators: `1,234` / `12,345`.
fn format_tok(n: u64) -> String {
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

fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= x + max_w {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Paint a whole row blank (keeping the given background style). The app
/// draws its left-aligned "~$co-sh" header at the top row, and `render_background`
/// only tints cells without clearing their glyph, so we must blank the title
/// row or the logo would bleed through the panel.
fn clear_row(buf: &mut Buffer, x: u16, y: u16, width: u16, style: Style) {
    for cx in x..x.saturating_add(width) {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(' ');
            cell.set_style(style);
        }
    }
}

/// Render the dashboard into `area` (the left panel rect).
pub fn render(buf: &mut Buffer, area: Rect, data: &DashboardData, theme: &Theme) {
    let mut bg = BoxRenderable::new();
    bg.set_background_color(Some(theme.background_panel.into()));
    bg.render_self(buf, area);

    let text = rgba_color(theme.text);
    let muted = rgba_color(theme.text_muted);
    let primary = rgba_color(theme.primary);
    let border = rgba_color(theme.border);

    let panel_bg = Style::default().bg(rgba_color(theme.background_panel));
    let inner = area.x + 2;
    // Amounts are right-aligned in a narrow column that, on the skinny
    // panel, sits right next to the (short) provider names.
    let amt_x = area.right().saturating_sub(9);
    let name_w = amt_x.saturating_sub(inner).saturating_sub(1);

    // ── Title (matches the session sidebar) ──
    clear_row(buf, area.x, area.y, area.width, panel_bg);
    draw_text(
        buf,
        " Usage",
        inner - 1,
        area.y,
        area.width,
        Style::default().fg(muted),
    );
    if let Some(cell) = buf.cell_mut((area.x + 1, area.y + 1)) {
        cell.set_char('\u{2500}');
        cell.set_style(Style::default().fg(border));
    }

    // ── Current session ──
    let mut y = area.y + 3;
    draw_text(
        buf,
        "CURRENT SESSION",
        inner,
        y,
        area.width,
        Style::default().fg(muted),
    );
    y += 2;
    draw_text(
        buf,
        &format!("tokens  {}", format_tok(data.session_tokens)),
        inner,
        y,
        area.width,
        Style::default().fg(text),
    );
    y += 1;
    // Cost policy: only the provider's REAL reported cost is shown. A
    // provider without cost reporting gets a muted `—` — no estimate is
    // ever fabricated, and the absence is NOT a warning (it is the
    // documented behavior for providers that don't vouch for a price).
    let (cost_str, cost_style) = match data.session_cost {
        Some(c) => (format_usd(c), Style::default().fg(text)),
        None => ("—".to_string(), Style::default().fg(muted)),
    };
    draw_text(
        buf,
        &format!("cost     {cost_str}"),
        inner,
        y,
        area.width,
        cost_style,
    );
    y += 2;

    // ── Period + per-provider spend ──
    // "PERIOD" followed by a colored highlight chip on the period name. The
    // chip background both delimiters the value (so no ":" is needed) and
    // shifts color per period so the active range is instantly readable.
    let period_rgba = period_highlight(data.period_enum, theme);
    let period_bg = rgba_color(period_rgba);
    let (pr, pg, pb, _) = period_rgba.to_ints();
    let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
    let chip_fg = if lum > 0.5 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    };
    draw_text(
        buf,
        "PERIOD",
        inner,
        y,
        area.width,
        Style::default().fg(muted),
    );
    let chip = format!(" {} ", data.period_enum.label());
    let chip_start = inner + 7; // "PERIOD" is 6 chars + 1 gap
    for (i, ch) in chip.chars().enumerate() {
        let cx = chip_start + i as u16;
        if cx < area.right()
            && let Some(cell) = buf.cell_mut((cx, y))
        {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(chip_fg).bg(period_bg));
        }
    }
    y += 1;
    let provider_rows: Vec<(String, f64)> = data
        .period
        .by_provider
        .iter()
        .map(|(p, c)| (p.clone(), *c))
        .collect();
    // Keep provider rows from running over the pinned Total at the bottom.
    let budget_bottom = area.bottom().saturating_sub(3);
    for (name, cost) in &provider_rows {
        if y >= budget_bottom {
            break;
        }
        draw_text(buf, name, inner, y, name_w, Style::default().fg(text));
        draw_text(
            buf,
            &format_usd(*cost),
            amt_x,
            y,
            9,
            Style::default().fg(primary),
        );
        y += 1;
    }

    // ── Total (pinned to bottom, same amount column) ──
    let total_y = area.bottom().saturating_sub(2);
    draw_text(
        buf,
        "Total",
        inner,
        total_y,
        name_w,
        Style::default().fg(text),
    );
    draw_text(
        buf,
        &format_usd(data.period.total),
        amt_x,
        total_y,
        9,
        Style::default().fg(primary),
    );
}
