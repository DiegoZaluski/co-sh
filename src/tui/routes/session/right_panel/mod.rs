use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme::Theme;

pub mod pty;
pub mod todo;
pub mod types;

use todo::{render_todo_section, todo_section_height};
use pty::render_pty_section;
use types::RightPanelState;

pub fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// How many PTYs we attempt to show at most in the panel, depending on available height.
const MAX_PTY_LINES_RUNNING: u16 = 8;
const MAX_PTY_LINES_DONE: u16 = 3;
const TODO_MIN_LINES: u16 = 2;

/// Panel width in columns.
pub const RIGHT_PANEL_WIDTH: u16 = 42;

/// Determine whether the right panel should be visible based on terminal width and content.
pub fn should_show_right_panel(terminal_width: u16, state: &RightPanelState) -> bool {
    // Always hide if terminal is too narrow
    if terminal_width < 100 {
        return false;
    }
    // Hide if no content
    if state.todos.is_empty() && state.pty_sessions.is_empty() {
        return false;
    }
    true
}

/// Determine the visible area thresholds for responsive PTY count.
/// Returns (max_ptys, show_todo).
fn compute_panel_layout(
    panel_height: u16,
    state: &RightPanelState,
) -> (usize, bool) {
    let has_todos = !state.todos.is_empty();

    // Available height for PTYs after accounting for TODO
    let todo_h = if has_todos {
        todo_section_height(&state.todos, RIGHT_PANEL_WIDTH.saturating_sub(4))
            .min(panel_height.saturating_sub(MAX_PTY_LINES_RUNNING))
    } else {
        0
    };

    let pty_avail = panel_height.saturating_sub(todo_h);

    // Determine max PTYs based on available height
    // Each PTY: running=8 lines, completed=3 lines
    let mut max_ptys = 0usize;
    let mut remaining = pty_avail as i32;

    for pty in &state.pty_sessions {
        let needed = if matches!(pty.status, types::PtyStatus::Running) {
            MAX_PTY_LINES_RUNNING as i32
        } else {
            MAX_PTY_LINES_DONE as i32
        };
        if remaining >= needed {
            remaining -= needed;
            max_ptys += 1;
        } else {
            break;
        }
    }

    // Don't show TODO if there are too many PTYs and not enough space
    let show_todo = if has_todos && max_ptys == state.pty_sessions.len() && remaining >= TODO_MIN_LINES as i32 {
        true
    } else if has_todos && max_ptys < state.pty_sessions.len() {
        // PTYs overflow — give space to PTYs, hide TODO
        false
    } else if has_todos {
        // Enough space for both
        todo_h <= panel_height.saturating_sub((max_ptys as u16).saturating_mul(MAX_PTY_LINES_DONE).max(MAX_PTY_LINES_RUNNING))
    } else {
        false
    };

    (max_ptys, show_todo)
}

/// Render the right panel.
pub fn render_right_panel(
    buf: &mut Buffer,
    area: Rect,
    state: &RightPanelState,
    theme: &Theme,
    terminal_width: u16,
) {
    if !should_show_right_panel(terminal_width, state) {
        return;
    }

    // Background
    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_panel.into()));
    bg_box.render_self(buf, area);

    // Draw a subtle left border
    let border_color = rgba_color(theme.border);
    for y in area.y..area.bottom() {
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_style(Style::default().fg(border_color));
            cell.set_char('▕');
        }
    }

    // Header
    let header_style = Style::default().fg(rgba_color(theme.text_muted));
    if let Some(cell) = buf.cell_mut((area.x + 2, area.y)) {
        cell.set_char('◧');
        cell.set_style(header_style);
    }

    let inner_x = area.x + 2;
    let inner_w = area.width.saturating_sub(4);
    let mut y = area.y + 1;
    let bottom = area.bottom();

    let (max_ptys, show_todo) = compute_panel_layout(bottom.saturating_sub(y), state);

    // Render TODO section (lower priority)
    if show_todo {
        let todo_max_h = bottom.saturating_sub(y).saturating_sub(MAX_PTY_LINES_RUNNING);
        if todo_max_h >= TODO_MIN_LINES {
            let todo_used = render_todo_section(
                buf,
                inner_x,
                y,
                inner_w,
                todo_max_h,
                &state.todos,
                theme,
            );
            y += todo_used;
        }
    }

    // Render PTY section (higher priority)
    if max_ptys > 0 {
        let pty_area = Rect::new(inner_x, y, inner_w, bottom.saturating_sub(y));
        render_pty_section(
            buf,
            pty_area,
            &state.pty_sessions,
            max_ptys,
            theme,
        );
    }
}
