use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::theme::Theme;
use super::types::TodoItem;
use super::rgba_color;

/// Render the TODO section. Returns the number of lines used.
pub fn render_todo_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    todos: &[TodoItem],
    theme: &Theme,
) -> u16 {
    if todos.is_empty() || max_h < 2 {
        return 0;
    }

    // Section header
    let header_style = Style::default().fg(rgba_color(theme.text));
    draw_text(buf, "Todos", x, y, max_w, header_style);
    let mut used = 1u16;

    let mut line_y = y + 1;
    let bottom = y + max_h;

    for todo in todos {
        if line_y >= bottom {
            break;
        }

        let (symbol, fg_color) = match todo.status.as_str() {
            "in_progress" => ("•", rgba_color(theme.warning)),
            "completed" => ("✓", rgba_color(theme.text_muted)),
            _ => (" ", rgba_color(theme.text_muted)),
        };

        let checkbox_style = Style::default().fg(fg_color);
        let text_style = Style::default().fg(if todo.status == "in_progress" {
            rgba_color(theme.warning)
        } else {
            rgba_color(theme.text_muted)
        });

        // Checkbox
        let label = format!("[{symbol}]");
        draw_text(buf, &label, x, line_y, max_w.min(4), checkbox_style);

        // Content (truncated)
        let content_x = x + 4;
        let content_w = max_w.saturating_sub(4);
        if content_w > 0 {
            let display = if todo.content.chars().count() > content_w as usize {
                let truncated: String = todo.content.chars().take(content_w.saturating_sub(1) as usize).collect();
                format!("{}…", truncated)
            } else {
                todo.content.clone()
            };
            draw_text(buf, &display, content_x, line_y, content_w, text_style);
        }

        line_y += 1;
        used += 1;
    }

    // Separator after todos
    if line_y < bottom {
        let sep_style = Style::default().fg(rgba_color(theme.border));
        if let Some(cell) = buf.cell_mut((x, line_y)) {
            cell.set_char('─');
            cell.set_style(sep_style);
        }
        if let Some(cell) = buf.cell_mut((x + max_w.saturating_sub(1), line_y)) {
            cell.set_char('─');
            cell.set_style(sep_style);
        }
        used += 1;
    }

    used
}

/// Estimate the height needed for the todo section.
pub fn todo_section_height(todos: &[TodoItem], _max_w: u16) -> u16 {
    if todos.is_empty() {
        return 0;
    }
    // Header + each todo + separator
    (todos.len() as u16).min(10) + 2
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
