use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub struct QuestionDialog {
    pub visible: bool,
    pub prompt: String,
    pub input: String,
    pub cursor: usize,
}

impl QuestionDialog {
    pub fn new() -> Self {
        QuestionDialog {
            visible: false,
            prompt: String::new(),
            input: String::new(),
            cursor: 0,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let dialog_w = 60.min(area.width.saturating_sub(4));
        let dialog_h = 7;
        let dialog_x = area.x + (area.width - dialog_w) / 2;
        let dialog_y = area.y + (area.height - dialog_h) / 2;
        let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

        let mut bg = BoxRenderable::new();
        bg.set_background_color(Some(theme.background_element.into()));
        bg.set_border_color(Some(theme.border_active.into()));
        bg.render_self(buf, dialog_area);

        let prompt_style = Style::default().fg(rgba_color(theme.text));
        let display_prompt = if self.prompt.is_empty() {
            "Question:"
        } else {
            &self.prompt
        };
        draw_text_line(
            buf,
            display_prompt,
            dialog_x + 2,
            dialog_y + 1,
            dialog_w.saturating_sub(4),
            prompt_style,
        );

        let input_display = if self.input.is_empty() {
            "Type your answer..."
        } else {
            &self.input
        };
        let input_style = if self.input.is_empty() {
            Style::default().fg(rgba_color(theme.text_muted))
        } else {
            Style::default().fg(rgba_color(theme.text))
        };

        let mut input_bg = BoxRenderable::new();
        input_bg.set_background_color(Some(theme.background_panel.into()));
        let input_area = Rect::new(dialog_x + 2, dialog_y + 3, dialog_w.saturating_sub(4), 1);
        input_bg.render_self(buf, input_area);
        draw_text_line(
            buf,
            input_display,
            dialog_x + 3,
            dialog_y + 3,
            dialog_w.saturating_sub(6),
            input_style,
        );

        let hint = "Enter submit  Esc cancel";
        draw_text_line(
            buf,
            hint,
            dialog_x + 2,
            dialog_y + dialog_h - 1,
            dialog_w.saturating_sub(2),
            Style::default().fg(rgba_color(theme.text_muted)),
        );
    }
}
