//! Scroll-to-bottom pill (`⮟`): shared by the session chat and the
//! `/agents` panes. Pure view over `(scroll_y, max_scroll)` plus its own
//! slide/fade animation state — no dependency on markdown, streaming or
//! chat layout. The caller supplies the anchor row; the pill centers
//! itself on the given area and reports its hit rect while active.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::theme::Theme;
use crate::theme::rgba_color;

/// Content rows between the viewport bottom and the content end at which
/// the pill appears.
pub const APPEAR_ROWS: i32 = 3;
const POP_IN_SECS: f32 = 0.12;
const FADE_OUT_SECS: f32 = 0.06;
const SLIDE_ROWS: f32 = 2.0;

#[derive(Debug, Clone, Default)]
pub struct ScrollPill {
    /// Screen rect last drawn while active (`None` while hidden/retracting).
    /// The mouse dispatcher hit-tests THIS.
    pub area: Option<Rect>,
    /// 0..=1 slide/opacity progress (1 = settled).
    pub progress: f32,
}

impl ScrollPill {
    #[must_use]
    pub fn new() -> Self {
        Self {
            area: None,
            progress: 0.0,
        }
    }

    /// Whether the slide/fade animation is mid-flight (drives the
    /// live-render gate so the animation completes without other input).
    #[must_use]
    pub fn animating(&self) -> bool {
        self.progress > 0.0 && self.progress < 1.0
    }

    /// Animation progress (0 hidden … 1 settled).
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.progress
    }

    /// Drop the hit rect and park the animation at rest (e.g. while a
    /// dialog hides the pill's row).
    pub fn clear(&mut self) {
        self.area = None;
        self.progress = 0.0;
    }

    /// Whether the viewport is far enough from the bottom to show the pill.
    #[must_use]
    pub fn should_show(scroll_y: i32, max_scroll: i32) -> bool {
        max_scroll - scroll_y.min(max_scroll) > APPEAR_ROWS
    }

    /// Draw the pill centered on `area` at `anchor_y` whenever the viewport
    /// is away from the latest content, advancing the animation by the real
    /// frame delta. `scroll_y`/`max_scroll` are in content rows.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        buf: &mut Buffer,
        scroll_y: i32,
        max_scroll: i32,
        anchor_y: u16,
        area: Rect,
        theme: &Theme,
        delta_time: f64,
    ) {
        let active = Self::should_show(scroll_y, max_scroll);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let dt = delta_time.max(0.0) as f32;
        if !active && self.progress > 0.0 {
            self.progress = (self.progress - dt / FADE_OUT_SECS).max(0.0);
        } else if active && self.progress < 1.0 {
            self.progress = (self.progress + dt / POP_IN_SECS).min(1.0);
        }
        let t = self.progress;
        if t <= 0.0 {
            self.area = None;
            return;
        }
        let pad_x = 2u16;
        let pill_w = 1 + pad_x * 2;
        if area.width < pill_w || area.height < 2 {
            self.area = None;
            return;
        }
        let cx = area.x + area.width.saturating_sub(pill_w) / 2;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let slide = ((1.0 - t) * (1.0 - t) * SLIDE_ROWS).round() as u16;
        let y = anchor_y.saturating_sub(slide).max(area.y);
        self.area = active.then(|| Rect::new(cx, y, pill_w, 1));

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let blend = |fg: RGBA, bg: RGBA| -> RGBA {
            let (fr, fg_g, fb, _) = fg.to_ints();
            let (br, bg_g, bb, _) = bg.to_ints();
            let mix = |f: u8, b: u8| (f32::from(f) * t + f32::from(b) * (1.0 - t)).round() as u8;
            RGBA::from_ints(mix(fr, br), mix(fg_g, bg_g), mix(fb, bb), 255)
        };
        let glyph_fg = blend(theme.primary, theme.background);
        let style = Style::default()
            .fg(rgba_color(glyph_fg))
            .add_modifier(Modifier::BOLD);
        if let Some(cell) = buf.cell_mut((cx.saturating_add(pad_x), y)) {
            cell.set_char('\u{2B9F}');
            cell.set_style(style);
        }
    }

    /// Hit-test a click against the last drawn rect.
    #[must_use]
    pub fn hit(&self, x: u16, y: u16) -> bool {
        self.area.is_some_and(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
    }
}
