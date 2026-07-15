use ratatui::buffer::Buffer;
use ratatui::style::Style;

use super::rgba_color;
use super::types::{PtySession, PtyStatus};
use crate::theme::Theme;

pub const PTY_MAX_LINES: usize = 200;

/// Compute the height (in terminal rows) that a PTY entry occupies.
pub fn pty_entry_height(session: &PtySession) -> u16 {
    let output_lines = session.output.lines().count().min(PTY_MAX_LINES);
    1 + output_lines as u16
}

/// Render a single PTY entry at the given position.
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
        let output_style = Style::default().fg(rgba_color(theme.text_muted));
        let output_x = x + 2;
        let output_w = max_w.saturating_sub(2);

        let available = (max_h.saturating_sub(1)) as usize;
        let all_lines: Vec<&str> = session.output.lines().collect();
        let total = all_lines.len().min(PTY_MAX_LINES);
        let shown = available.min(total);

        for (i, line) in all_lines.iter().take(shown).enumerate() {
            let truncated: String = line.chars().take(output_w as usize).collect();
            draw_text(
                buf,
                &truncated,
                output_x,
                y + 1 + i as u16,
                output_w,
                output_style,
            );
        }
    }
}

fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
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
