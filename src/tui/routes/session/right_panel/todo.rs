use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::rgba_color;
use super::types::TodoItem;
use crate::theme::Theme;

/// Gap above the box (1 blank line).
const TOP_GAP: u16 = 1;
/// Internal top padding inside the box (1 blank line).
const TOP_PAD: u16 = 1;
/// Internal bottom padding inside the box (1 blank line).
const BOTTOM_PAD: u16 = 1;
/// Internal left padding inside the box (1 column).
const LEFT_PAD: u16 = 1;

/// Render the TODO section. Returns the number of lines used.
pub fn render_todo_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    todos: &[TodoItem],
    theme: &Theme,
    scroll_y: Option<&mut i32>,
) -> u16 {
    // Need at least: gap(1) + top_pad(1) + header(1) + 1 item + bottom_pad(1) = 5
    if todos.is_empty() || max_h < TOP_GAP + TOP_PAD + 1 + 1 + BOTTOM_PAD {
        return 0;
    }

    // How many items can actually fit
    let box_overhead = TOP_PAD + 1 + BOTTOM_PAD; // top_pad + header + bottom_pad
    let max_visible = (max_h.saturating_sub(TOP_GAP + box_overhead)) as usize;
    if max_visible == 0 {
        return 0;
    }

    // Apply scroll offset for the TODO items
    let scroll_offset = scroll_y.map_or(0, |s| {
        let max_scroll = todos.len().saturating_sub(max_visible);
        *s = (*s).min(max_scroll as i32).max(0);
        *s as usize
    });
    let visible_items = max_visible.min(todos.len().saturating_sub(scroll_offset));

    let box_h = box_overhead + visible_items as u16; // full box height
    let box_y = y + TOP_GAP; // box starts after the gap
    let total_used = TOP_GAP + box_h; // total lines from section start

    // Fill box background
    let todo_area = Rect::new(x, box_y, max_w, box_h);
    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_element.into()));
    bg_box.render_self(buf, todo_area);

    // Internal top padding (blank line)
    let mut line_y = box_y + TOP_PAD;

    // Section header — left-aligned with left padding
    let header_style = Style::default().fg(rgba_color(theme.text));
    draw_text(
        buf,
        "Todos",
        x + LEFT_PAD,
        line_y,
        max_w.saturating_sub(LEFT_PAD),
        header_style,
    );
    line_y += 1;

    // Each todo item — left-aligned
    let bottom_edge = y + total_used - BOTTOM_PAD;
    for (i, todo) in todos.iter().skip(scroll_offset).enumerate() {
        if i >= visible_items || line_y >= bottom_edge {
            break;
        }

        let (symbol, fg_color) = match todo.status.as_str() {
            "in_progress" => ("•", rgba_color(theme.warning)),
            "completed" => ("✓", rgba_color(theme.text_muted)),
            _ => (" ", rgba_color(theme.text_muted)),
        };

        let checkbox = format!("[{}]", symbol);
        let checkbox_w = checkbox.chars().count() as u16;

        // Truncate content if needed
        let max_text_w = max_w.saturating_sub(LEFT_PAD + checkbox_w + 2);
        let display_text = if todo.content.chars().count() > max_text_w as usize && max_text_w > 1 {
            let truncated: String = todo
                .content
                .chars()
                .take(max_text_w.saturating_sub(1) as usize)
                .collect();
            format!("{}…", truncated)
        } else {
            todo.content.clone()
        };

        // Left-aligned with left padding
        let item_x = x + LEFT_PAD;
        draw_text(
            buf,
            &checkbox,
            item_x,
            line_y,
            checkbox_w,
            Style::default().fg(fg_color),
        );

        let content_x = item_x + checkbox_w + 1;
        let content_max_w = max_w.saturating_sub(content_x - x);
        if content_max_w > 0 {
            draw_text(
                buf,
                &display_text,
                content_x,
                line_y,
                content_max_w,
                Style::default().fg(if todo.status == "in_progress" {
                    rgba_color(theme.warning)
                } else {
                    rgba_color(theme.text_muted)
                }),
            );
        }

        line_y += 1;
    }

    total_used
}

/// Estimate the height needed for the todo section.
pub fn todo_section_height(todos: &[TodoItem], _max_w: u16) -> u16 {
    if todos.is_empty() {
        return 0;
    }
    // gap + (top_pad + header + items + bottom_pad)
    // Cap at 30 items to avoid extreme natural_h values.
    TOP_GAP + TOP_PAD + 1 + (todos.len() as u16).min(30) + BOTTOM_PAD
}

fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
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
