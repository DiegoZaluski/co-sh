use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Deny,
    AllowOnce,
}

pub struct PermissionRequest {
    pub tool: String,
    pub description: String,
    pub args: String,
}

pub struct PermissionDialog {
    pub visible: bool,
    pub request: Option<PermissionRequest>,
    pub selected: usize,
}

impl PermissionDialog {
    pub fn new() -> Self {
        PermissionDialog {
            visible: false,
            request: None,
            selected: 0,
        }
    }

    /// Handle a mouse click on the permission dialog.
    /// Returns the action if the user clicked an option, or None if outside/not visible.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect, _theme: &Theme) -> Option<PermissionAction> {
        if !self.visible {
            return None;
        }

        let dialog_w = 50.min(area.width.saturating_sub(4));
        let dialog_h = 10.min(area.height.saturating_sub(4));
        let dialog_x = area.x + (area.width - dialog_w) / 2;
        let dialog_y = area.y + (area.height - dialog_h) / 2;

        let x = mouse.x;
        let y_click = mouse.y;

        // Check if click is within dialog area
        if x < dialog_x || x >= dialog_x + dialog_w || y_click < dialog_y || y_click >= dialog_y + dialog_h {
            return None;
        }

        let options = ["Allow", "Deny", "Allow Once"];
        for (i, _opt) in options.iter().enumerate() {
            let oy = dialog_y + 5 + i as u16;
            if y_click == oy {
                self.selected = i;
                let action = match i {
                    0 => PermissionAction::Allow,
                    1 => PermissionAction::Deny,
                    _ => PermissionAction::AllowOnce,
                };
                return Some(action);
            }
        }

        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let dialog_w = 50.min(area.width.saturating_sub(4));
        let dialog_h = 10.min(area.height.saturating_sub(4));
        let dialog_x = area.x + (area.width - dialog_w) / 2;
        let dialog_y = area.y + (area.height - dialog_h) / 2;
        let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

        let mut border = BoxRenderable::new();
        border.set_background_color(Some(theme.background_element.into()));
        border.set_border_color(Some(theme.border_active.into()));
        border.render_self(buf, dialog_area);

        if let Some(request) = &self.request {
            let title_style = Style::default().fg(rgba_color(theme.text));
            draw_text_line(
                buf,
                &request.tool,
                dialog_x + 2,
                dialog_y + 1,
                dialog_w.saturating_sub(4),
                title_style,
            );

            let desc_style = Style::default().fg(rgba_color(theme.text_muted));
            draw_text_line(
                buf,
                &request.description,
                dialog_x + 2,
                dialog_y + 2,
                dialog_w.saturating_sub(4),
                desc_style,
            );

            let args_style = Style::default().fg(rgba_color(theme.warning));
            draw_text_line(
                buf,
                &request.args,
                dialog_x + 2,
                dialog_y + 3,
                dialog_w.saturating_sub(4),
                args_style,
            );

            let options = ["Allow", "Deny", "Allow Once"];
            for (i, opt) in options.iter().enumerate() {
                let oy = dialog_y + 5 + i as u16;
                let prefix = if i == self.selected {
                    "\u{25b8} "
                } else {
                    "  "
                };
                let text = format!("{prefix}{opt}");
                let style = if i == self.selected {
                    Style::default().fg(rgba_color(theme.primary))
                } else {
                    Style::default().fg(rgba_color(theme.text_muted))
                };
                draw_text_line(
                    buf,
                    &text,
                    dialog_x + 3,
                    oy,
                    dialog_w.saturating_sub(6),
                    style,
                );
            }

            let hint = "\u{2191}\u{2195} navigate  Enter confirm  Esc cancel";
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
