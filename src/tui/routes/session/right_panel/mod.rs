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

/// Count visible sections and return a bitmask: todo, bash, subagent.
fn count_sections(state: &RightPanelState) -> (bool, bool, bool) {
    let has_todos = !state.todos.is_empty();
    let has_bash = state
        .pty_sessions
        .iter()
        .any(|p| !p.command.starts_with("subagent:"));
    let has_subagent = state
        .pty_sessions
        .iter()
        .any(|p| p.command.starts_with("subagent:"));
    (has_todos, has_bash, has_subagent)
}

/// Compute the natural height (in rows) that a section would occupy if unlimited.
fn natural_section_height(state: &RightPanelState, inner_w: u16, kind: types::SectionKind) -> i32 {
    match kind {
        types::SectionKind::Todo => {
            if state.todos.is_empty() {
                0
            } else {
                todo_section_height(&state.todos, inner_w) as i32
            }
        }
        types::SectionKind::Bash => state
            .pty_sessions
            .iter()
            .filter(|p| !p.command.starts_with("subagent:"))
            .map(|p| pty_entry_height(p) as i32)
            .sum(),
        types::SectionKind::Subagent => state
            .pty_sessions
            .iter()
            .rposition(|p| p.command.starts_with("subagent:"))
            .map(|i| pty_entry_height(&state.pty_sessions[i]) as i32)
            .unwrap_or(0),
    }
}

/// Render the right panel with section-based layout and dynamic space borrowing.
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
    let viewport_h = area.bottom() as i32 - viewport_top;

    state.visible_height = viewport_h;

    let (has_todo, has_bash, has_subagent) = count_sections(state);
    let visible_count = has_todo as i32 + has_bash as i32 + has_subagent as i32;
    if visible_count == 0 {
        return;
    }

    let base_h = viewport_h / visible_count;
    let mut pool = 0i32;
    let mut cur_y = viewport_top;

    // ── Helper: allocate a section ─────────────────────────────────
    let allocate_section = |
        buf: &mut Buffer,
        state: &mut RightPanelState,
        kind: types::SectionKind,
        theme: &Theme,
        inner_x: u16,
        inner_w: u16,
        cur_y: &mut i32,
        pool: &mut i32,
        available_h: i32,
    | {
        let natural_h = natural_section_height(state, inner_w, kind);

        // How much this section can actually use (base + borrow from pool)
        let allocated = if natural_h <= available_h {
            // Section uses less than its base → surplus goes to pool
            let surplus = available_h - natural_h;
            *pool += surplus;
            natural_h
        } else {
            // Section needs more → borrow from pool if available
            let extra = (natural_h - available_h).min(*pool);
            *pool -= extra;
            available_h + extra
        };

        let allocated = allocated.max(1); // at least 1 row

        match kind {
            types::SectionKind::Todo => {
                let scroll = if natural_h > allocated {
                    // Not all content fits, need scroll
                    state.todo_scroll_y = state
                        .todo_scroll_y
                        .min((natural_h - allocated) as i32);
                    Some(&mut state.todo_scroll_y)
                } else {
                    None
                };
                render_todo_section(
                    buf,
                    inner_x,
                    *cur_y as u16,
                    inner_w,
                    allocated as u16,
                    &state.todos,
                    theme,
                    scroll,
                );
            }
            types::SectionKind::Bash => {
                render_bash_section(
                    buf,
                    inner_x,
                    *cur_y as u16,
                    inner_w,
                    allocated as u16,
                    state,
                    theme,
                );
            }
            types::SectionKind::Subagent => {
                render_subagent_section(
                    buf,
                    inner_x,
                    *cur_y as u16,
                    inner_w,
                    allocated as u16,
                    state,
                    theme,
                );
            }
        }

        *cur_y += allocated;
    };

    if has_todo {
        allocate_section(
            buf, state, types::SectionKind::Todo, theme,
            inner_x, inner_w, &mut cur_y, &mut pool, base_h,
        );
    }
    if has_bash {
        allocate_section(
            buf, state, types::SectionKind::Bash, theme,
            inner_x, inner_w, &mut cur_y, &mut pool, base_h,
        );
    }
    if has_subagent {
        allocate_section(
            buf, state, types::SectionKind::Subagent, theme,
            inner_x, inner_w, &mut cur_y, &mut pool, base_h,
        );
    }
}

/// Render all bash PTYs into a single section.
fn render_bash_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    let bash_ptys: Vec<usize> = state
        .pty_sessions
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.command.starts_with("subagent:"))
        .map(|(i, _)| i)
        .collect();

    if bash_ptys.is_empty() {
        return;
    }

    // Calculate total natural height
    let natural_h: i32 = bash_ptys
        .iter()
        .map(|&i| pty_entry_height(&state.pty_sessions[i]) as i32)
        .sum();

    let has_scroll = natural_h > max_h as i32;
    let scroll_y = if has_scroll {
        state.bash_scroll_y = state
            .bash_scroll_y
            .min((natural_h - max_h as i32) as i32);
        state.bash_scroll_y
    } else {
        0
    };

    // Virtual scroll within the bash section
    let mut cur_y = y as i32 - scroll_y;
    let viewport_bottom = (y + max_h) as i32;

    for &idx in &bash_ptys {
        let pty = &state.pty_sessions[idx];
        let pty_h = pty_entry_height(pty) as i32;

        if cur_y + pty_h > y as i32 && cur_y < viewport_bottom {
            let render_y = cur_y.max(y as i32) as u16;
            let clip_h = (cur_y + pty_h).min(viewport_bottom) - render_y as i32;
            render_one_pty(
                buf,
                x,
                render_y,
                max_w,
                clip_h as u16,
                pty,
                theme,
            );
        }
        cur_y += pty_h;
    }

    // Draw a small scrollbar if content overflows
    if has_scroll {
        draw_section_scrollbar(buf, x + max_w, y, max_h, natural_h, scroll_y, theme);
    }
}

/// Render subagent PTYs into a single section with chunk-based scrolling.
fn render_subagent_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    // Find the last (most recent) subagent PTY
    let sub_idx = state
        .pty_sessions
        .iter()
        .rposition(|p| p.command.starts_with("subagent:"));
    let Some(idx) = sub_idx else {
        return;
    };

    let session = &state.pty_sessions[idx];
    let natural_h = pty_entry_height(session) as i32;
    let max_content_h = (max_h as i32 - 1).max(1) as usize; // -1 for command line

    let output_lines: Vec<&str> = session.output.lines().collect();
    let total_lines = output_lines.len();

    if total_lines == 0 {
        render_one_pty(buf, x, y, max_w, max_h, session, theme);
        return;
    }

    // If content fits, render directly
    if natural_h <= max_h as i32 {
        state.subagent_chunk_index = 0;
        state.subagent_chunk_count = 1;
        state.subagent_chunk_capacity = max_content_h;
        render_one_pty(buf, x, y, max_w, max_h, session, theme);
        return;
    }

    // Content doesn't fit — split into chunks
    let chunk_count = total_lines.div_ceil(max_content_h);
    state.subagent_chunk_capacity = max_content_h;

    if state.subagent_chunk_count == 0 {
        // First render — start at chunk 0 so we don't skip initial output.
        state.subagent_chunk_index = 0;
    } else if chunk_count > state.subagent_chunk_count {
        // New output arrived, advance to next chunk (smooth transition).
        state.subagent_chunk_index =
            (state.subagent_chunk_index + 1).min(chunk_count.saturating_sub(1));
    }
    state.subagent_chunk_count = chunk_count;

    let chunk = state.subagent_chunk_index.min(chunk_count.saturating_sub(1));
    let start = chunk * max_content_h;
    let end = (start + max_content_h).min(total_lines);

    // Build a "virtual" PTY with just this chunk's output
    let chunk_output: String = output_lines[start..end].join("\n");
    let mut virtual_pty = session.clone();
    virtual_pty.output = chunk_output;

    render_one_pty(buf, x, y, max_w, max_h, &virtual_pty, theme);

    // Show chunk indicator
    if chunk_count > 1 {
        let indicator = format!(
            " [{}/{}]",
            chunk + 1,
            chunk_count
        );
        let style = Style::default().fg(rgba_color(theme.text_muted));
        draw_simple_text(
            buf,
            &indicator,
            x + max_w.saturating_sub(indicator.chars().count() as u16 + 1),
            y,
            indicator.chars().count() as u16,
            style,
        );
    }
}

/// Minimal text drawing helper (no control-char filtering, used for indicators).
fn draw_simple_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
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

/// Draw a scrollbar within a section (not the full panel).
fn draw_section_scrollbar(
    buf: &mut Buffer,
    sb_x: u16,
    sb_y: u16,
    sb_h: u16,
    content_h: i32,
    scroll_y: i32,
    theme: &Theme,
) {
    let sb_h = sb_h as i32;
    let thumb_size = (sb_h as f64 * sb_h as f64 / content_h as f64)
        .max(1.0)
        .min(sb_h as f64) as i32;

    let max_thumb_pos = sb_h - thumb_size;
    let scroll_ratio = if content_h > sb_h {
        scroll_y as f64 / (content_h - sb_h) as f64
    } else {
        0.0
    };
    let thumb_pos = (scroll_ratio * max_thumb_pos as f64) as i32;

    let sb_style = Style::default().fg(rgba_color(theme.text_muted));

    for row in 0..sb_h {
        let y = sb_y + row as u16;
        if let Some(cell) = buf.cell_mut((sb_x, y)) {
            if row >= thumb_pos && row < thumb_pos + thumb_size {
                cell.set_char('█');
                cell.set_style(sb_style);
            }
        }
    }
}
