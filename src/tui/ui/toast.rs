use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use cosh_tui::core::lib::border::BorderSidesConfig;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::styled_text::string_to_styled_text;
use cosh_tui::core::lib::unicode_util;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::types::TextAttributes;

use crate::theme::Theme;

#[derive(Debug, Clone)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastVariant {
    const fn theme_color(&self, theme: &Theme) -> RGBA {
        match self {
            Self::Info => theme.info,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Error => theme.error,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToastOptions {
    pub title: Option<String>,
    pub message: String,
    pub variant: ToastVariant,
    pub duration_ms: u64,
}

impl Default for ToastOptions {
    fn default() -> Self {
        Self {
            title: None,
            message: String::new(),
            variant: ToastVariant::Info,
            duration_ms: 5000,
        }
    }
}

pub struct ToastState {
    pub current: Option<ToastOptions>,
    pub elapsed: u64,
}

impl ToastState {
    pub const fn new() -> Self {
        Self {
            current: None,
            elapsed: 0,
        }
    }

    pub fn show(&mut self, options: ToastOptions) {
        self.current = Some(options);
        self.elapsed = 0;
    }

    pub fn tick(&mut self, dt: u64) {
        if let Some(ref toast) = self.current {
            self.elapsed += dt;
            if self.elapsed >= toast.duration_ms {
                self.current = None;
                self.elapsed = 0;
            }
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let Some(toast) = &self.current else { return };

        // Width hugs the widest content line (plus horizontal padding),
        // still capped at 60 columns and by the available area width.
        let padding_x = 2u16;
        let max_w = 60u16.min(area.width.saturating_sub(6));
        let content_w = toast
            .title
            .as_deref()
            .map(unicode_util::str_display_width)
            .unwrap_or(0)
            .max(unicode_util::str_display_width(&toast.message));
        let toast_w = content_w
            .saturating_add((padding_x * 2) as usize)
            .min(max_w as usize) as u16;
        let toast_x = area.right().saturating_sub(toast_w + 2);
        let toast_y = area.y + 2;

        let border_color = toast.variant.theme_color(theme);

        let mut bg = BoxRenderable::new();
        bg.set_background_color(Some(theme.background_panel.into()));
        bg.set_border_color(Some(border_color.into()));
        bg.set_border_sides(BorderSidesConfig {
            left: true,
            right: true,
            top: false,
            bottom: false,
        });

        let inner_h = 3u16;
        let toast_area = Rect::new(toast_x, toast_y, toast_w, inner_h);
        bg.render_self(buf, toast_area);

        let inner_w = toast_w.saturating_sub(padding_x * 2);

        let mut y = toast_y;
        if let Some(ref title) = toast.title {
            let mut t = TextRenderable::new(Some(string_to_styled_text(title)));
            t.set_fg(theme.text);
            t.set_bg(theme.background_panel);
            t.set_attributes(TextAttributes::BOLD.bits());
            let title_area = Rect::new(toast_x + padding_x, y, inner_w, 1);
            t.render_self(buf, title_area);
            y += 1;
        } else {
            // No title: skip one line so the message appears centered.
            y += 1;
        }

        let mut msg = TextRenderable::new(Some(string_to_styled_text(&toast.message)));
        msg.set_fg(theme.text);
        msg.set_bg(theme.background_panel);
        let msg_area = Rect::new(toast_x + padding_x, y, inner_w, 1);
        msg.render_self(buf, msg_area);
    }
}

impl Default for ToastState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(duration_ms: u64) -> ToastOptions {
        ToastOptions {
            title: Some("t".to_string()),
            message: "m".to_string(),
            variant: ToastVariant::Info,
            duration_ms,
        }
    }

    /// Regression: during the agent loop frames can be far apart (streaming
    /// render throttling, blocking draws) and `handle_events()` may not run
    /// at all between them. The toast must dismiss once the CUMULATIVE real
    /// deltas reach `duration_ms`, no matter how sparse the ticks are.
    #[test]
    fn dismisses_after_cumulative_real_deltas() {
        let mut state = ToastState::new();
        state.show(toast(5_000));

        // Streaming-like frames: ~30 fps with occasional multi-hundred-ms
        // draw stalls. Each tick stays below the duration, so the toast
        // must remain visible after every one of them.
        for dt in [33, 34, 300, 33, 34, 200, 33] {
            state.tick(dt);
            assert!(
                state.current.is_some(),
                "toast must outlive a {dt} ms tick below its duration"
            );
        }

        // 667 ms elapsed so far — crossing the threshold dismisses it.
        state.tick(4_333);
        assert!(state.current.is_none(), "toast must dismiss at duration");
        assert_eq!(state.elapsed, 0, "elapsed resets on dismissal");
    }

    /// A new toast replaces the previous one and its lifetime restarts
    /// from zero (never inherits the previous toast's elapsed time).
    #[test]
    fn show_resets_elapsed() {
        let mut state = ToastState::new();
        state.show(toast(1_000));
        state.tick(900);

        state.show(toast(1_000));
        assert_eq!(state.elapsed, 0);

        state.tick(999);
        assert!(state.current.is_some(), "fresh toast must outlive 999 ms");
        state.tick(1);
        assert!(state.current.is_none());
    }

    /// Ticking with no active toast is a no-op (keeps elapsed at zero).
    #[test]
    fn tick_without_toast_is_noop() {
        let mut state = ToastState::new();
        state.tick(10_000);
        assert!(state.current.is_none());
        assert_eq!(state.elapsed, 0);
    }

    /// The toast box hugs its content instead of using the old hardcoded
    /// 60-column width: the text column position reveals the box geometry
    /// (text x = toast_x + padding, toast_x = area.right() - (w + margin)).
    #[test]
    fn width_fits_content() {
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        let area = Rect::new(0, 0, 80, 10);

        // Short message: content_w = 1, padding 2*2, right margin 2 →
        // toast_w = 5, box starts at column 73, text at 75.
        let mut state = ToastState::new();
        state.show(toast(5_000)); // title "t", message "m"
        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme);
        let msg_col = (0..area.width)
            .find(|&x| buf[(x, 3)].symbol() == "m")
            .expect("message rendered");
        assert_eq!(msg_col, 75, "short toast must hug its content");

        // Long message caps at 60 columns: box starts at column 18,
        // text at 20.
        let mut long = toast(5_000);
        long.message = "x".repeat(100);
        let mut state = ToastState::new();
        state.show(long);
        let mut buf = Buffer::empty(area);
        state.render(&mut buf, area, &theme);
        let msg_col = (0..area.width)
            .find(|&x| buf[(x, 3)].symbol() == "x")
            .expect("long message rendered");
        assert_eq!(msg_col, 20, "long toast must cap at 60 columns");
    }
}
