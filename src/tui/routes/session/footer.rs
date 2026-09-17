use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::state::AppState;
use crate::theme::{Theme, rgba_color};

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    let mut cx = x;
    for ch in text.chars() {
        // Wide glyphs (CJK, emoji — all legal in branch names and paths)
        // occupy two terminal cells; advancing one cell per char would
        // shift the right-anchored segments and overwrite the wide char's
        // second half.
        let w = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(0)
            .max(1) as u16;
        if cx + w > right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        if w == 2
            && let Some(cell) = buf.cell_mut((cx + 1, y))
        {
            cell.set_char(' ');
            cell.set_style(style);
        }
        cx += w;
    }
}

pub struct FooterView;

/// Label of the footer's export button — the mouse twin of the `/export`
/// slash command. Its display width is also the hit-test width in the mouse
/// dispatcher (`App::handle_mouse_event`), so both must reference THIS
/// constant instead of re-deriving the string.
pub const EXPORT_BUTTON_LABEL: &str = " ⤓ export";

/// Rect of the export button inside the footer row, for click hit-testing.
/// `None` when the footer shows no session segments (no active session),
/// matching the render gate exactly.
pub fn export_button_rect(area: Rect, state: &AppState) -> Option<Rect> {
    state.current_session()?;
    let width = unicode_width::UnicodeWidthStr::width(EXPORT_BUTTON_LABEL).max(1) as u16;
    // Rightmost segment: `render_with_mode` anchors the right-anchored group
    // at `area.right() - 2` and draws the export button FIRST.
    let right = area.right().saturating_sub(2);
    Some(Rect::new(right.saturating_sub(width), area.y, width, 1))
}

impl FooterView {
    pub fn render_with_mode(
        buf: &mut Buffer,
        area: Rect,
        state: &AppState,
        theme: &Theme,
        hide_text: bool,
    ) {
        let bg_color = rgba_color(theme.background);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_style(Style::default().bg(bg_color));
                cell.set_char(' ');
            }
        }

        // When hide_text is true (e.g. question dialog is visible), just render the background
        if hide_text {
            return;
        }

        let muted = Style::default().fg(rgba_color(theme.text_muted));
        let success = Style::default().fg(rgba_color(theme.success));
        let warning = Style::default().fg(rgba_color(theme.warning));
        let accent = Style::default().fg(rgba_color(theme.accent));

        if state.current_session().is_some() {
            let dir = &state.working_directory;
            let dir_display = if dir.is_empty() { "~" } else { dir };

            // Display width, not byte length: right-anchored segments and
            // the draw limit must both count terminal cells (wide glyphs
            // occupy two).
            let disp_w = |s: &str| unicode_width::UnicodeWidthStr::width(s).max(1) as u16;

            let mut rx = area.right().saturating_sub(2);

            // The `/export` button: rightmost footer segment, so the
            // hit-test rect in `export_button_rect` stays a pure function of
            // the label width (independent of branch/dir segment lengths).
            let export_str = EXPORT_BUTTON_LABEL;
            rx = rx.saturating_sub(disp_w(export_str));
            draw_text_line(buf, export_str, rx, area.y, disp_w(export_str), accent);

            let dir_str = format!(" {dir_display}");
            rx = rx.saturating_sub(disp_w(&dir_str));
            draw_text_line(buf, &dir_str, rx, area.y, disp_w(&dir_str), muted);

            // Current git branch (or detached SHA) beside the working
            // directory; hidden entirely outside a repository.
            if let Some(branch) = &state.git_branch {
                let branch_str = format!(" {branch}");
                rx = rx.saturating_sub(disp_w(&branch_str));
                draw_text_line(buf, &branch_str, rx, area.y, disp_w(&branch_str), accent);
            }

            let conn_indicator = if state.connected {
                "\u{25cf}"
            } else {
                "\u{25cb}"
            };
            let conn_str = format!(" {conn_indicator}");
            rx = rx.saturating_sub(disp_w(&conn_str));
            let conn_style = if state.connected { success } else { warning };
            draw_text_line(buf, &conn_str, rx, area.y, disp_w(&conn_str), conn_style);

            if state.permission_count > 0 {
                let s = format!("  perm {}", state.permission_count);
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line(buf, &s, rx, area.y, disp_w(&s), muted);
            }

            if state.mcp_count > 0 || state.mcp_errors > 0 {
                let s = if state.mcp_errors > 0 {
                    format!("  mcp {}/{}", state.mcp_count, state.mcp_errors)
                } else {
                    format!("  mcp {}", state.mcp_count)
                };
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line(buf, &s, rx, area.y, disp_w(&s), muted);
            }

            if !state.lsp_servers.is_empty() {
                let s = format!("  lsp {}", state.lsp_servers.len());
                rx = rx.saturating_sub(disp_w(&s));
                draw_text_line(buf, &s, rx, area.y, disp_w(&s), muted);
            }
        }
    }
}
