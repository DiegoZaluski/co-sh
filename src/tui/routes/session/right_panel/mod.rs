use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;

use crate::theme::Theme;

pub mod pty;
pub mod todo;
pub mod types;

use pty::{pty_entry_height, render_one_pty};
use todo::{render_todo_section, todo_section_height};
use types::RightPanelState;

pub fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Panel width in columns.
pub const RIGHT_PANEL_WIDTH: u16 = 42;

/// Determine whether the right panel should be visible based on terminal width and content.
pub fn should_show_right_panel(terminal_width: u16, state: &RightPanelState) -> bool {
    if terminal_width < 100 {
        return false;
    }
    if state.todos.is_empty() && state.pty_sessions.is_empty() {
        return false;
    }
    true
}

/// Compute total content height for all sections.
fn compute_content_height(state: &RightPanelState, inner_w: u16) -> i32 {
    let mut h = 0i32;
    if !state.todos.is_empty() {
        h += todo_section_height(&state.todos, inner_w) as i32;
    }
    for pty in &state.pty_sessions {
        h += pty_entry_height(pty) as i32;
    }
    h
}

/// Render the right panel with virtual scrolling support.
pub fn render_right_panel(
    buf: &mut Buffer,
    area: Rect,
    state: &mut RightPanelState,
    theme: &Theme,
    terminal_width: u16,
) {
    if !should_show_right_panel(terminal_width, state) {
        return;
    }

    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_panel.into()));
    bg_box.render_self(buf, area);

    let inner_x = area.x + 2;
    let inner_w = area.width.saturating_sub(4);
    let viewport_top = area.y as i32;
    let viewport_bottom = area.bottom() as i32;
    let viewport_h = viewport_bottom - viewport_top;

    // Compute total content height
    let total_h = compute_content_height(state, inner_w);
    state.content_height = total_h;
    state.set_visible_height(viewport_h);

    // Virtual y-position of the content top
    let mut cur_virtual_y = viewport_top - state.scroll_y;

    // ── TODO section ──────────────────────────────────────────────
    if !state.todos.is_empty() {
        let todo_h = todo_section_height(&state.todos, inner_w) as i32;
        if is_visible(cur_virtual_y, todo_h, viewport_top, viewport_bottom) {
            let render_y = cur_virtual_y.max(viewport_top) as u16;
            let clip_h = (cur_virtual_y + todo_h).min(viewport_bottom) - render_y as i32;
            render_todo_section(
                buf,
                inner_x,
                render_y,
                inner_w,
                clip_h as u16,
                &state.todos,
                theme,
            );
        }
        cur_virtual_y += todo_h;
    }

    // ── PTY sessions ──────────────────────────────────────────────
    for pty in &state.pty_sessions {
        let pty_h = pty_entry_height(pty) as i32;
        if is_visible(cur_virtual_y, pty_h, viewport_top, viewport_bottom) {
            let render_y = cur_virtual_y.max(viewport_top) as u16;
            let clip_h = (cur_virtual_y + pty_h).min(viewport_bottom) - render_y as i32;
            render_one_pty(buf, inner_x, render_y, inner_w, clip_h as u16, pty, theme);
        }
        cur_virtual_y += pty_h;
    }

    // ── Scrollbar ─────────────────────────────────────────────────
    if total_h > viewport_h {
        draw_scrollbar(buf, area, state, viewport_h, theme);
    }
}

/// Check if an entry occupies any visible rows within the viewport.
fn is_visible(entry_top: i32, entry_h: i32, vp_top: i32, vp_bottom: i32) -> bool {
    entry_top + entry_h > vp_top && entry_top < vp_bottom
}

/// Draw a simple scrollbar on the right edge.
fn draw_scrollbar(
    buf: &mut Buffer,
    area: Rect,
    state: &RightPanelState,
    viewport_h: i32,
    theme: &Theme,
) {
    let sb_x = area.right().saturating_sub(1);
    let sb_h = viewport_h;

    let thumb_size = (sb_h as f64 * viewport_h as f64 / state.content_height as f64)
        .max(1.0)
        .min(sb_h as f64) as i32;

    let max_thumb_pos = sb_h - thumb_size;
    let scroll_ratio = if state.content_height > viewport_h {
        state.scroll_y as f64 / (state.content_height - viewport_h) as f64
    } else {
        0.0
    };
    let thumb_pos = (scroll_ratio * max_thumb_pos as f64) as i32;

    let sb_style = Style::default().fg(rgba_color(theme.text_muted));

    for row in 0..sb_h {
        let y = area.y + row as u16;
        if let Some(cell) = buf.cell_mut((sb_x, y)) {
            if row >= thumb_pos && row < thumb_pos + thumb_size {
                cell.set_char('█');
                cell.set_style(sb_style);
            }
        }
    }
}
