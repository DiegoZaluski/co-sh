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


use todo::{render_todo_section, todo_section_height};
use types::RightPanelState;

pub fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

/// Panel width in columns.
pub const RIGHT_PANEL_WIDTH: u16 = 42;
/// Gap (in rows) between consecutive sections.
const SECTION_GAP: i32 = 1;

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
        types::SectionKind::Bash => bash_buffer_lines(state).len() as i32,
        types::SectionKind::Subagent => subagent_buffer_lines(state).len() as i32,
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

    // Reserve space for gaps between visible sections
    let gap_total = (visible_count - 1) * SECTION_GAP;
    let available_for_content = viewport_h - gap_total;
    let base_h = available_for_content / visible_count;
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
                        .min(natural_h - allocated);
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

    let sections_list = [
        (has_todo, types::SectionKind::Todo),
        (has_bash, types::SectionKind::Bash),
        (has_subagent, types::SectionKind::Subagent),
    ];

    for (i, &(visible, kind)) in sections_list.iter().enumerate() {
        if !visible {
            continue;
        }
        allocate_section(
            buf, state, kind, theme,
            inner_x, inner_w, &mut cur_y, &mut pool, base_h,
        );
        // Add a blank gap after every section except the last visible one
        let remaining = sections_list[i + 1..].iter().any(|&(v, _)| v);
        if remaining {
            cur_y += SECTION_GAP;
        }
    }
}

/// Build a continuous buffer of all subagent PTY command + output lines, in order.
/// Same-agent entries are deduplicated by `app.rs` (retain by prefix).
fn subagent_buffer_lines(state: &RightPanelState) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for pty in &state.pty_sessions {
        if !pty.command.starts_with("subagent:") {
            continue;
        }
        // Header: the command line (e.g., "subagent: opencode")
        lines.push(pty.command.clone());
        // Output lines
        for line in pty.output.lines() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// Build a continuous buffer of all bash command + output lines, in order.
fn bash_buffer_lines(state: &RightPanelState) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for pty in &state.pty_sessions {
        if pty.command.starts_with("subagent:") {
            continue;
        }
        // Header: $ command (simulating a shell prompt)
        lines.push(format!("$ {}", pty.command));
        // Output lines
        for line in pty.output.lines() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// Render all bash PTYs as a single continuous text buffer with line-based scroll.
#[allow(clippy::too_many_arguments)]
fn render_bash_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    let buffer = bash_buffer_lines(state);
    if buffer.is_empty() {
        return;
    }

    let total_lines = buffer.len() as i32;
    let has_scroll = total_lines > max_h as i32;
    let scroll_y = if has_scroll {
        state.bash_scroll_y = state
            .bash_scroll_y
            .min(total_lines - max_h as i32);
        state.bash_scroll_y
    } else {
        0
    };

    let start_line = scroll_y as usize;
    let visible = max_h as usize;

    for (i, line) in buffer.iter().skip(start_line).take(visible).enumerate() {
        let line_y = y + i as u16;
        // Command lines ($ cmd) in normal text, output in muted
        let line_style = if line.starts_with("$ ") {
            Style::default().fg(rgba_color(theme.text))
        } else {
            Style::default().fg(rgba_color(theme.text_muted))
        };
        let truncated: String = line.chars().take(max_w as usize).collect();
        draw_text(buf, &truncated, x, line_y, max_w, line_style);
    }

    if has_scroll {
        draw_section_scrollbar(buf, x + max_w, y, max_h, total_lines, scroll_y, theme);
    }
}

/// Render all subagent PTYs as a single continuous text buffer with line-based scroll.
///
/// Different agent CLIs (e.g. opencode + kilo) stack; the same agent called
/// again replaces its previous entry (handled in `app.rs` via `retain`).
/// Lines with "→ cosh:" prefix are the main agent's input (normal style);
/// all other output lines use muted style.
#[allow(clippy::too_many_arguments)]
fn render_subagent_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    state: &mut RightPanelState,
    theme: &Theme,
) {
    let buffer = subagent_buffer_lines(state);
    if buffer.is_empty() {
        return;
    }

    let total_lines = buffer.len() as i32;
    let has_scroll = total_lines > max_h as i32;
    let scroll_y = if has_scroll {
        state.subagent_scroll_y = state
            .subagent_scroll_y
            .min(total_lines - max_h as i32);
        state.subagent_scroll_y
    } else {
        0
    };

    let start_line = scroll_y as usize;
    let visible = max_h as usize;

    for (i, line) in buffer.iter().skip(start_line).take(visible).enumerate() {
        let line_y = y + i as u16;
        // Command headers and "→ cosh:" input lines in normal text;
        // subagent response lines in muted text.
        let line_style = if line.starts_with("subagent:") || line.starts_with("→ cosh:") {
            Style::default().fg(rgba_color(theme.text))
        } else {
            Style::default().fg(rgba_color(theme.text_muted))
        };
        let truncated: String = line.chars().take(max_w as usize).collect();
        draw_text(buf, &truncated, x, line_y, max_w, line_style);
    }

    if has_scroll {
        draw_section_scrollbar(buf, x + max_w, y, max_h, total_lines, scroll_y, theme);
    }
}

/// Simple text drawing helper (filters ASCII control chars to prevent ratatui panics).
fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().filter(|c| !c.is_ascii_control()).enumerate() {
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
#[allow(clippy::too_many_arguments)]
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
        if let Some(cell) = buf.cell_mut((sb_x, y))
            && row >= thumb_pos && row < thumb_pos + thumb_size
        {
            cell.set_char('█');
            cell.set_style(sb_style);
        }
    }
}
