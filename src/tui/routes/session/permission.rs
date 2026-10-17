use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use cosh_tui::core::types::MouseEvent;

use crate::component::wall;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;

const OPTIONS: [&str; 3] = ["Allow", "Allow Once", "Deny"];

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

/// One row of a permission box's options, computed from the option labels.
/// The pane-scoped boxes reuse the main dialog's exact geometry: the shared
/// `PermissionDialog` render needs no new painter.
pub struct PanePermissionDialog {
    /// The box's heading tool row.
    pub tool: String,
    /// The human description (wraps).
    pub description: String,
    /// The args row (path, command, …).
    pub args: String,
    /// The option labels, in the agent's offered order.
    pub options: Vec<String>,
    /// The options' semantic kinds (allow kind green / reject red /
    /// other amber), parallel to `options` — the agent labels its own
    /// rows, so the KIND (not the label) is the color's source of truth.
    pub option_kinds: Vec<String>,
    /// The highlighted row.
    pub selected: usize,
}

impl PanePermissionDialog {
    /// Required height — the SAME arithmetic `PermissionDialog::required_
    /// height` runs for its fixed three options, generalized to `options`.
    #[must_use]
    pub fn required_height(&self, max_width: u16) -> u16 {
        let inner_w = max_width.saturating_sub(5); // pad(3) + 2
        let desc_lines = wrap_lines(&self.description, inner_w);
        1 + desc_lines + 1 + self.options.len() as u16 + 1
    }

    /// Paint the box: byte-for-byte the main dialog's painter, with the
    /// option count generalized (the color rule reads the option's kind:
    /// allow rows green, reject rows red, the rest warning amber).
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let theme_text_muted = rgba_color(theme.text_muted);
        let theme_warning = rgba_color(theme.warning);
        let theme_bg_element = rgba_color(theme.background_element);

        wall::render(buf, area, theme.accent, theme.background_panel);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let chars_per_line = inner_w as usize;
        let mut y_pos = area.y + 1;

        // Tool name row (the main dialog carries it inside the description;
        // the split's request keeps it separate like the event payload).
        draw_text_line(
            buf,
            &self.tool,
            inner_x,
            y_pos,
            inner_w,
            Style::default().fg(theme_warning),
        );
        y_pos += 1;

        // Description (may wrap across multiple lines).
        let desc_lines = wrap_lines(&self.description, inner_w) as usize;
        for li in 0..desc_lines {
            let line: String = self
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

        // Row: Args.
        draw_text_line(
            buf,
            &self.args,
            inner_x,
            y_pos,
            inner_w,
            Style::default().fg(theme_warning),
        );
        y_pos += 1;

        // Rows: the agent's own options, each colored by its semantic kind
        // (allow green / reject red / other amber) — the KIND is the color's
        // source of truth, never the agent's label.
        for (i, opt) in self.options.iter().enumerate() {
            let is_selected = i == self.selected;
            let kind = self
                .option_kinds
                .get(i)
                .map(String::as_str)
                .unwrap_or_default();

            // Active row highlight (matches the question dialog style). The
            // band stops one column short of the right edge so the right
            // `│` border stays visible on the highlighted row.
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
            let style = if is_selected {
                Style::default().fg(rgba_color(theme.accent))
            } else if kind.starts_with("allow") {
                Style::default().fg(rgba_color(theme.success))
            } else if kind.starts_with("reject") {
                Style::default().fg(rgba_color(theme.error))
            } else {
                Style::default().fg(theme_text_muted)
            };
            draw_text_line(buf, &text, inner_x, y_pos, inner_w, style);
            y_pos += 1;
        }
    }
}

impl PanePermissionDialog {
    /// Resolve a mouse click inside `area` to the clicked option row
    /// (0-based), mirroring the main dialog's row arithmetic (the same
    /// height contract). `None` outside the box or below/above any option.
    #[must_use]
    pub fn clicked_option(&self, mouse_y: u16, area: Rect) -> Option<usize> {
        if mouse_y < area.y || mouse_y >= area.y + area.height {
            return None;
        }
        let inner_w = area.width.saturating_sub(5);
        let desc_lines = wrap_lines(&self.description, inner_w);
        let first_option_y = area.y + 2 + desc_lines; // tool row + desc + args
        let idx = mouse_y.checked_sub(first_option_y)? as usize;
        (idx < self.options.len()).then_some(idx)
    }
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

        wall::render(buf, area, theme.accent, theme.background_panel);

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
                // `│` border stays visible on the highlighted row.
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
