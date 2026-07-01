use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::lib::border::BorderSidesConfig;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::styled_text::string_to_styled_text;
use cosh_tui::core::types::TextAttributes;

use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

#[derive(Debug, Clone)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastVariant {
    fn theme_color(&self, theme: &Theme) -> RGBA {
        match self {
            ToastVariant::Info => theme.info,
            ToastVariant::Success => theme.success,
            ToastVariant::Warning => theme.warning,
            ToastVariant::Error => theme.error,
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
        ToastOptions {
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
    pub fn new() -> Self {
        ToastState {
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
        let toast = match &self.current {
            Some(t) => t,
            None => return,
        };

        let max_w = 60u16.min(area.width.saturating_sub(6));
        let toast_w = max_w;
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

        let inner_h = if toast.title.is_some() { 3u16 } else { 2u16 };
        let toast_area = Rect::new(toast_x, toast_y, toast_w, inner_h);
        bg.render_self(buf, toast_area);

        let mut y = toast_y;
        if let Some(ref title) = toast.title {
            let mut t = TextRenderable::new(Some(string_to_styled_text(title)));
            t.set_fg(theme.text);
            t.set_attributes(TextAttributes::BOLD.bits());
            let title_area = Rect::new(toast_x + 1, y, toast_w.saturating_sub(2), 1);
            t.render_self(buf, title_area);
            y += 1;
        }

        let mut msg = TextRenderable::new(Some(string_to_styled_text(&toast.message)));
        msg.set_fg(theme.text);
        msg.set_bg(theme.background_panel);
        let msg_area = Rect::new(toast_x + 1, y, toast_w.saturating_sub(2), 1);
        msg.render_self(buf, msg_area);
    }
}

impl Default for ToastState {
    fn default() -> Self {
        Self::new()
    }
}
