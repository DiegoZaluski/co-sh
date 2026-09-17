use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};

const OPTIONS: [&str; 3] = ["Allow", "Allow Once", "Deny"];

const fn left_border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '\u{2503}',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '\u{2503}',
        right_t: ' ',
        cross: ' ',
    }
}

/// Estimate how many lines a text wraps to at a given width.
fn wrap_lines(text: &str, width: u16) -> u16 {
    if width < 10 {
        text.chars().count().max(1) as u16
    } else {
        text.chars().count().div_ceil(width as usize).max(1) as u16
    }
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
    pub const fn new() -> Self {
        Self {
            visible: false,
            request: None,
            selected: 0,
        }
    }

    /// Calculate the required height for the dialog.
    /// Grows upward (y adjusts in app.rs) so content never disappears off the bottom.
    pub fn required_height(&self, max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        let inner_w = max_width.saturating_sub(5); // pad(3) + 2
        let desc_lines = self
            .request
            .as_ref()
            .map_or(1, |r| wrap_lines(&r.description, inner_w));
        // 1 top padding + desc_lines + 1 args + 3 options + 1 bottom padding
        1 + desc_lines + 1 + 3 + 1
    }

    /// Handle a mouse click on the permission dialog.
    /// Returns the action if the user clicked an option, or None if outside/not visible.
    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        _theme: &Theme,
    ) -> Option<PermissionAction> {
        if !self.visible {
            return None;
        }

        let x = mouse.x;
        let y_click = mouse.y;

        // Check if click is within dialog area
        if x < area.x
            || x >= area.x + area.width
            || y_click < area.y
            || y_click >= area.y + area.height
        {
            return None;
        }

        // Options start after: 1 top padding + desc_lines + 1 args row
        let inner_w = area.width.saturating_sub(5);
        let desc_lines = self
            .request
            .as_ref()
            .map_or(1, |r| wrap_lines(&r.description, inner_w));
        let first_option_y = area.y + 1 + desc_lines + 1;

        for (i, _opt) in OPTIONS.iter().enumerate() {
            let oy = first_option_y + i as u16;
            if y_click == oy {
                self.selected = i;
                return Some(match i {
                    0 => PermissionAction::Allow,
                    1 => PermissionAction::AllowOnce,
                    _ => PermissionAction::Deny,
                });
            }
        }

        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let theme_text_muted = rgba_color(theme.text_muted);
        let theme_warning = rgba_color(theme.warning);
        let theme_success = rgba_color(theme.success);
        let theme_error = rgba_color(theme.error);
        let theme_bg_element = rgba_color(theme.background_element);

        // Left-border bar with panel background (matches question dialog style)
        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.accent.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: true,
            bottom: false,
        });
        border_box.set_custom_border_chars(left_border_chars());
        border_box.render_self(buf, area);

        if let Some(request) = &self.request {
            let pad = 3u16;
            let inner_x = area.x + pad;
            let inner_w = area.width.saturating_sub(pad + 2);
            let chars_per_line = inner_w as usize;
            let mut y_pos = area.y + 1;

            // Description (may wrap across multiple lines)
            let desc_lines = wrap_lines(&request.description, inner_w) as usize;
            for li in 0..desc_lines {
                let line: String = request
                    .description
                    .chars()
                    .skip(li * chars_per_line)
                    .take(chars_per_line)
                    .collect();
                draw_text_line(
                    buf,
                    &line,
                    inner_x,
                    y_pos,
                    inner_w,
                    Style::default().fg(theme_text_muted),
                );
                y_pos += 1;
            }

            // Row: Args (path, command, etc.)
            draw_text_line(
                buf,
                &request.args,
                inner_x,
                y_pos,
                inner_w,
                Style::default().fg(theme_warning),
            );
            y_pos += 1;

            // Rows: Options (Allow, Allow Once, Deny)
            let option_colors = [theme_success, theme_warning, theme_error];
            for (i, opt) in OPTIONS.iter().enumerate() {
                let is_selected = i == self.selected;

                // Active row highlight (matches question dialog style). The
                // band stops one column short of the right edge so the right
                // `┃` border stays visible on the highlighted row.
                if is_selected {
                    for cx in area.x + 1..area.x + area.width.saturating_sub(1) {
                        if let Some(cell) = buf.cell_mut((cx, y_pos)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(theme_bg_element));
                        }
                    }
                }

                let prefix = if is_selected { "\u{1F7B4} " } else { "  " };
                let text = format!("{prefix}{opt}");
                let color = option_colors[i];
                let style = if is_selected {
                    Style::default().fg(color)
                } else {
                    Style::default().fg(theme_text_muted)
                };
                draw_text_line(buf, &text, inner_x, y_pos, inner_w, style);
                y_pos += 1;
            }
        }
    }
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters (the dialog renders model-provided tool
        // descriptions/args): writing them into cells makes ratatui's buffer
        // diff panic ("control character passed to cell_width without
        // filtering").
        if ch.is_control() {
            continue;
        }
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
