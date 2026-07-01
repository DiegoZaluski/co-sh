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

#[derive(Debug, Clone)]
pub enum DialogType {
    Alert { message: String },
    Confirm { message: String },
}

#[derive(Debug, Clone)]
pub struct DialogInstance {
    pub dialog_type: DialogType,
    pub selected: usize,
}

pub struct DialogState {
    pub stack: Vec<DialogInstance>,
}

impl DialogState {
    pub fn new() -> Self {
        DialogState {
            stack: Vec::new(),
        }
    }

    pub fn show(&mut self, dialog_type: DialogType) {
        self.stack.push(DialogInstance {
            dialog_type,
            selected: 0,
        });
    }

    pub fn replace(&mut self, dialog_type: DialogType) {
        self.clear();
        self.show(dialog_type);
    }

    pub fn pop(&mut self) {
        self.stack.pop();
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn visible(&self) -> bool {
        !self.stack.is_empty()
    }

    pub fn current(&self) -> Option<&DialogInstance> {
        self.stack.last()
    }

    pub fn current_mut(&mut self) -> Option<&mut DialogInstance> {
        self.stack.last_mut()
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let instance = match self.stack.last() {
            Some(i) => i,
            None => return,
        };

        let dialog_w = 50.min(area.width.saturating_sub(4));
        let dialog_x = area.x + (area.width - dialog_w) / 2;

        match &instance.dialog_type {
            DialogType::Alert { message } => {
                let dialog_h = 5;
                let dialog_y = area.y + (area.height - dialog_h) / 2;
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let mut bg = BoxRenderable::new();
                bg.set_background_color(Some(theme.background_element.into()));
                bg.set_border_color(Some(theme.border_active.into()));
                bg.render_self(buf, dialog_area);

                draw_text_line(buf, message, dialog_x + 2, dialog_y + 1, dialog_w.saturating_sub(4), Style::default().fg(rgba_color(theme.text)));

                let ok_text = "[ OK ]";
                let ok_x = dialog_x + (dialog_w - ok_text.len() as u16) / 2;
                let ok_style = Style::default().fg(rgba_color(theme.primary));
                draw_text_line(buf, ok_text, ok_x, dialog_y + 3, dialog_w.saturating_sub(2), ok_style);
            }
            DialogType::Confirm { message } => {
                let dialog_h = 6;
                let dialog_y = area.y + (area.height - dialog_h) / 2;
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let mut bg = BoxRenderable::new();
                bg.set_background_color(Some(theme.background_element.into()));
                bg.set_border_color(Some(theme.border_active.into()));
                bg.render_self(buf, dialog_area);

                draw_text_line(buf, message, dialog_x + 2, dialog_y + 1, dialog_w.saturating_sub(4), Style::default().fg(rgba_color(theme.text)));

                let options = ["Yes", "No"];
                for (i, opt) in options.iter().enumerate() {
                    let oy = dialog_y + 3 + i as u16;
                    let prefix = if i == instance.selected { "\u{25b8} " } else { "  " };
                    let text = format!("{}{}", prefix, opt);
                    let style = if i == instance.selected {
                        Style::default().fg(rgba_color(theme.primary))
                    } else {
                        Style::default().fg(rgba_color(theme.text_muted))
                    };
                    draw_text_line(buf, &text, dialog_x + 3, oy, dialog_w.saturating_sub(6), style);
                }
            }
        }
    }
}
