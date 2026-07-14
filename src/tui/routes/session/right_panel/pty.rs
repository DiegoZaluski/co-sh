use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme::Theme;
use super::types::{PtySession, PtyStatus};
use super::rgba_color;

const PTY_RUNNING_LINES: u16 = 8;
const PTY_DONE_LINES: u16 = 3;

/// Count how many PTYs can be visible given the available height.
pub fn count_visible_ptys(sessions: &[PtySession], avail_height: u16) -> usize {
    let mut count = 0usize;
    let mut remaining = avail_height as i32;

    for session in sessions {
        let needed = if matches!(session.status, PtyStatus::Running) {
            PTY_RUNNING_LINES as i32
        } else {
            PTY_DONE_LINES as i32
        };
        if remaining >= needed {
            remaining -= needed;
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Render a single PTY entry (command header + output).
fn render_one_pty(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    session: &PtySession,
    theme: &Theme,
) -> u16 {
    let is_running = matches!(session.status, PtyStatus::Running);
    let is_failed = matches!(session.status, PtyStatus::Failed);

    // Status icon
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

    // Command line
    let cmd_style = if is_running {
        Style::default().fg(icon_color)
    } else {
        Style::default().fg(rgba_color(theme.text_muted))
    };

    let cmd_label = format!("{} {}", icon, session.command);
    let cmd_truncated: String = cmd_label
        .chars()
        .take(max_w as usize)
        .collect();
    draw_text(buf, &cmd_truncated, x, y, max_w, cmd_style);

    if max_h <= 1 {
        return 1;
    }

    // Output lines (if any)
    if !session.output.is_empty() {
        let output_style = Style::default().fg(rgba_color(theme.text_muted));
        let output_x = x + 2;
        let output_w = max_w.saturating_sub(2);
        let max_output_lines = max_h.saturating_sub(1).min(
            if is_running { PTY_RUNNING_LINES - 1 } else { PTY_DONE_LINES - 1 }
        );
        let mut line_y = y + 1;

        for (i, line) in session.output.lines().enumerate() {
            if i >= max_output_lines as usize || line_y >= y + max_h {
                break;
            }
            let truncated: String = line
                .chars()
                .take(output_w as usize)
                .collect();
            draw_text(buf, &truncated, output_x, line_y, output_w, output_style);
            line_y += 1;
        }
    }

    max_h
}

/// Render the PTY section. `max_ptys` is the max number of PTYs to show.
pub fn render_pty_section(
    buf: &mut Buffer,
    area: Rect,
    sessions: &[PtySession],
    max_ptys: usize,
    theme: &Theme,
) {
    let mut y = area.y;
    let bottom = area.bottom();

    for session in sessions.iter().take(max_ptys) {
        if y >= bottom {
            break;
        }

        let pty_h = if matches!(session.status, PtyStatus::Running) {
            PTY_RUNNING_LINES
        } else {
            PTY_DONE_LINES
        }.min(bottom.saturating_sub(y));

        render_one_pty(
            buf,
            area.x,
            y,
            area.width,
            pty_h,
            session,
            theme,
        );

        y += pty_h;
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
