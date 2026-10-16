//! The SENT-prompt box: the `│`-railed frame a submitted user message
//! paints in the main chat — extracted verbatim from the session route's
//! `render_user_message` so the `/agents` pane can mirror the EXACT same
//! box for its inputs. One point of truth for both chats: a restyle of
//! the sent box changes this component, never a per-surface copy.
//!
//! Geometry (the chat's contract, byte-preserved):
//! - a `│` rail on EACH side (left + right), full height; no top/bottom
//!   border, no corners — the rails sit on the underlying surface;
//! - a background band (`background_panel`) painted one column short of
//!   each rail, so the rails read on the chat/pane background exactly
//!   like the prompt input box;
//! - the CONTENT is the caller's: the chat draws its parts at
//!   `x + 3` within `width - 6`; the pane blits its markdown block over
//!   the band. The frame only owns the chrome.

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::theme::Theme;

/// Side-rail border glyphs: `│` on both verticals, everything else blank —
/// the chat's sent-message gutter (the same glyphs the session route's
/// `left_border_chars` yields for a left+right box; the corners and
/// T-joints are never drawn because top/bottom are off).
const fn side_rail_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '│',
        top_t: ' ',
        bottom_t: ' ',
        left_t: ' ',
        right_t: ' ',
        cross: ' ',
    }
}

/// Paint the sent-prompt box frame over `area`: the `│` rails in
/// `rail_color` on both sides, the `theme.background_panel` band between
/// them (one column short of each rail). Call BEFORE painting the box's
/// content — the content overwrites the band, never the rails.
///
/// The rail TONE is the caller's speaker decision: the chat paints the
/// user's messages in the agent-mode color and errors in `theme.error`;
/// a pane paints the user's echo in the pane agent's brand color and
/// another agent's input in the theme's secondary — the color alone tells
/// WHO is speaking, the shape is shared.
pub(crate) fn render(buf: &mut Buffer, area: Rect, rail_color: RGBA, theme: &Theme) {
    let mut border_box = BoxRenderable::new();
    // The `│` rails sit on the chat background like the prompt box:
    // the panel band is painted separately, one column short of each rail.
    border_box.set_border_color(Some(rail_color.into()));
    border_box.set_border_sides(BorderSidesConfig {
        left: true,
        top: false,
        right: true,
        bottom: false,
    });
    border_box.set_custom_border_chars(side_rail_chars());
    border_box.render_self(buf, area);

    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_panel.into()));
    // The band stops one column short of the left/right edges so the `│`
    // rails sit on the chat background, mirroring the prompt box.
    let bg_area = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    bg_box.render_self(buf, bg_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    /// The extraction contract, byte-enforced: the component paints
    /// EXACTLY what the chat's inline construction painted — the border
    /// glyphs come from the session route's own `left_border_chars`, the
    /// band inset is the same one-column-short rect.
    #[test]
    fn frame_matches_the_chat_inline_construction_byte_for_byte() {
        let theme = theme();
        let area = Rect::new(4, 3, 40, 7);
        let rail = RGBA::from_ints(255, 100, 0, 255);

        let mut expected = Buffer::empty(Rect::new(0, 0, 60, 20));
        // ── the OLD inline construction, verbatim from render_user_message ──
        let mut border_box = BoxRenderable::new();
        border_box.set_border_color(Some(rail.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: true,
            bottom: false,
        });
        border_box.set_custom_border_chars(
            crate::routes::session::tool_render::left_border_chars(),
        );
        border_box.render_self(&mut expected, area);
        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_panel.into()));
        let bg_area = Rect::new(
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            area.height,
        );
        bg_box.render_self(&mut expected, bg_area);

        let mut actual = Buffer::empty(Rect::new(0, 0, 60, 20));
        render(&mut actual, area, rail, &theme);

        assert_eq!(actual, expected);
    }

    #[test]
    fn rails_run_the_full_height_and_the_band_stops_one_column_short() {
        let theme = theme();
        let area = Rect::new(2, 1, 30, 5);
        let rail = RGBA::from_ints(10, 200, 120, 255);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        render(&mut buf, area, rail, &theme);

        for dy in 0..area.height {
            let y = area.y + dy;
            let left = &buf[(area.x, y)];
            let right = &buf[(area.x + area.width - 1, y)];
            assert_eq!(left.symbol(), "│", "left rail row {dy}");
            assert_eq!(right.symbol(), "│", "right rail row {dy}");
            let expected_fg = ratatui::style::Color::Rgb(10, 200, 120);
            assert_eq!(left.fg, expected_fg, "left rail tone row {dy}");
            assert_eq!(right.fg, expected_fg, "right rail tone row {dy}");
        }
        // The band fills the inner columns (not the rail columns) for the
        // full height, with the theme's panel tone.
        let panel = theme.background_panel;
        for dy in 0..area.height {
            let y = area.y + dy;
            for dx in 1..area.width - 1 {
                let cell = &buf[(area.x + dx, y)];
                let (r, g, b, _) = panel.to_ints();
                assert_eq!(
                    cell.bg,
                    ratatui::style::Color::Rgb(r, g, b),
                    "band cell dx={dx} row {dy}"
                );
            }
        }
        // Outside the frame: untouched.
        assert_eq!(buf[(area.x - 1, area.y)].symbol(), " ");
        assert_eq!(buf[(area.x + area.width, area.y)].symbol(), " ");
    }

    #[test]
    fn a_one_row_frame_still_carries_both_rails() {
        let theme = theme();
        let area = Rect::new(0, 0, 10, 1);
        let rail = RGBA::from_ints(200, 200, 200, 255);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        render(&mut buf, area, rail, &theme);
        assert_eq!(buf[(0, 0)].symbol(), "│");
        assert_eq!(buf[(9, 0)].symbol(), "│");
    }
}
