use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::rgba_color;
use super::types::{PtySession, PtyStatus};
use crate::theme::Theme;

pub const PTY_MAX_LINES: usize = 200;

/// Compute the height (in terminal rows) that a PTY entry occupies.
pub fn pty_entry_height(session: &PtySession) -> u16 {
    let output_lines = session.output.lines().count().min(PTY_MAX_LINES);
    // +1 for the command line
    1 + output_lines as u16
}

/// Render a single PTY entry at the given position.
///
/// For subagent sessions (command starts with "subagent:"), the output
/// area is rendered inside a subtle box with a background fill, making
/// it visually distinct as a "chat" area.
///
/// `max_h` controls how many rows are available — the virtual scroll
/// system clips to the viewport, so we render as many lines as fit.
pub fn render_one_pty(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    session: &PtySession,
    theme: &Theme,
) {
    let is_running = matches!(session.status, PtyStatus::Running);
    let is_failed = matches!(session.status, PtyStatus::Failed);
    let is_subagent = session.command.starts_with("subagent:");

    let icon = if is_running {
        "▸"
    } else if is_failed {
        "✗"
    } else {
        "✓"
    };

    let icon_color = if is_running {
        rgba_color(theme.warning)
    } else if is_failed {
        rgba_color(theme.error)
    } else {
        rgba_color(theme.success)
    };

    let cmd_style = if is_running {
        Style::default().fg(icon_color)
    } else {
        Style::default().fg(rgba_color(theme.text_muted))
    };

    let cmd_label = format!("{} {}", icon, session.command);
    let cmd_truncated: String = cmd_label.chars().take(max_w as usize).collect();
    draw_text(buf, &cmd_truncated, x, y, max_w, cmd_style);

    if max_h <= 1 {
        return;
    }

    if !session.output.is_empty() {
        let output_box_h = max_h.saturating_sub(1);
        let output_x = x;
        let output_y = y + 1;
        let output_w = max_w;

        // Draw a background box for subagent entries to create a "chat" area
        if is_subagent {
            let mut bg = BoxRenderable::new();
            bg.set_background_color(Some(theme.background_element.into()));
            bg.render_self(buf, Rect::new(output_x, output_y, output_w, output_box_h));
        }

        // Render output text
        let output_style = Style::default().fg(rgba_color(theme.text_muted));
        let text_x = if is_subagent {
            output_x + 1
        } else {
            output_x + 2
        };
        let text_w = output_w.saturating_sub(2);

        let available = output_box_h as usize;
        let all_lines: Vec<&str> = session.output.lines().collect();
        let total = all_lines.len().min(PTY_MAX_LINES);
        let shown = available.min(total);

        for (i, line) in all_lines.iter().take(shown).enumerate() {
            // Lines starting with "→ cosh:" are the main agent's input message;
            // render them in the standard text color to visually distinguish
            // the prompt from the subagent's response.
            let line_style = if line.starts_with("→ cosh:") {
                Style::default().fg(rgba_color(theme.text))
            } else {
                output_style
            };
            let truncated: String = line.chars().take(text_w as usize).collect();
            draw_text(
                buf,
                &truncated,
                text_x,
                output_y + i as u16,
                text_w,
                line_style,
            );
        }
    }
}

/// Simple text drawing helper. Filters ASCII control chars to prevent
/// ratatui panics. Uses `checked_add` to avoid u16 overflow when
/// computing character positions.
fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        // Skip ASCII control characters (e.g. ESC \x1b from ANSI escape
        // sequences) which would cause ratatui issues.
        if ch.is_ascii_control() {
            continue;
        }
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}
