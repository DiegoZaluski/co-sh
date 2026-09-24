use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::types::{
    TodoItem, wrap_chars, TODO_STRIKE_HOLD_FRAMES, TODO_STRIKE_REVEAL_FRAMES,
    TODO_STRIKE_TOTAL_FRAMES,
};
use crate::theme::Theme;
use crate::theme::rgba_color;

/// Gap above the box (1 blank line).
const TOP_GAP: u16 = 1;
/// Internal top padding inside the box (1 blank line).
const TOP_PAD: u16 = 1;
/// Internal bottom padding inside the box (1 blank line).
const BOTTOM_PAD: u16 = 1;
/// Internal left padding inside the box (1 column).
const LEFT_PAD: u16 = 1;
/// Width of the status checkbox ("[•]", "[✓]", "[ ]").
const CHECKBOX_W: u16 = 3;

/// Content column width for a given section width: checkbox + one gap col,
/// after the left padding.
fn content_width(max_w: u16) -> u16 {
    max_w.saturating_sub(LEFT_PAD + CHECKBOX_W + 1)
}

/// Render the TODO section. Returns the number of lines used.
///
/// Item contents WRAP to the section width (continuation lines align under
/// the content column); scrolling is line-based across the flattened rows.
#[allow(clippy::too_many_arguments)]
// ═══════════════════════ Completion strike-through animation ═════════════════

/// Number of characters a completed item's label reveals crossed out at
/// `frame` during its strike animation. `None` = the animation has already
/// finished: render the permanent full strike. Mirrors oh-my-pi's
/// `strikeRevealCount`: the counter holds at 0 for `TODO_STRIKE_HOLD_FRAMES`
/// (2) frames so the un-struck row registers, then wipes left-to-right over
/// `TODO_STRIKE_REVEAL_FRAMES` (12) frames.
fn strike_reveal_count(label: &str, frame: u64) -> Option<usize> {
    if frame > u64::from(TODO_STRIKE_TOTAL_FRAMES) {
        return None;
    }
    let chars = label.chars().count();
    if chars == 0 {
        return None;
    }
    if frame <= u64::from(TODO_STRIKE_HOLD_FRAMES) {
        return Some(0);
    }
    let reveal_frame =
        (frame - u64::from(TODO_STRIKE_HOLD_FRAMES)).min(u64::from(TODO_STRIKE_REVEAL_FRAMES));
    let reveal = chars * usize::try_from(reveal_frame).unwrap_or(0)
        / usize::try_from(u64::from(TODO_STRIKE_REVEAL_FRAMES)).unwrap_or(1);
    Some(reveal.min(chars))
}

pub fn render_todo_section(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_h: u16,
    todos: &[TodoItem],
    strike_frames: &[Option<u64>],
    theme: &Theme,
    scroll_y: Option<&mut i32>,
) -> u16 {
    // Need at least: gap(1) + top_pad(1) + header(1) + bottom_pad(1).
    if todos.is_empty() || max_h < TOP_GAP + TOP_PAD + 1 + BOTTOM_PAD {
        return 0;
    }

    let content_max_w = content_width(max_w);

    // Flattened visual rows: (item index, wrapped segment). Long contents
    // break onto following lines instead of being cut with an ellipsis.
    let mut rows: Vec<(usize, String)> = Vec::new();
    for (i, todo) in todos.iter().take(30).enumerate() {
        for seg in wrap_chars(&todo.content, content_max_w) {
            rows.push((i, seg));
        }
        if rows.len() > 5000 {
            break; // safety valve for pathological plans
        }
    }

    let box_overhead = TOP_PAD + 1 + BOTTOM_PAD; // top_pad + header + bottom_pad
    let visible_rows = max_h.saturating_sub(TOP_GAP + box_overhead) as usize;
    if visible_rows == 0 || rows.is_empty() {
        return 0;
    }

    // Line-based scroll: the stored offset may carry the i32::MAX
    // scroll-to-bottom sentinel until a render clamps it.
    let scroll_row = scroll_y.map_or(0usize, |s| {
        let max_scroll = rows.len().saturating_sub(visible_rows);
        *s = (*s).min(max_scroll as i32).max(0);
        *s as usize
    });
    let shown = visible_rows.min(rows.len() - scroll_row);

    let box_h = box_overhead + shown as u16;
    let box_y = y + TOP_GAP;
    let total_used = TOP_GAP + box_h;
    let bottom_edge = y + total_used - BOTTOM_PAD;

    // Fill box background
    let mut bg_box = BoxRenderable::new();
    bg_box.set_background_color(Some(theme.background_element.into()));
    bg_box.render_self(buf, Rect::new(x, box_y, max_w, box_h));

    let mut line_y = box_y + TOP_PAD;

    // Section header — left-aligned with left padding
    draw_text(
        buf,
        "Todos",
        x + LEFT_PAD,
        line_y,
        max_w.saturating_sub(LEFT_PAD),
        Style::default().fg(rgba_color(theme.text)),
    );
    line_y += 1;

    let item_x = x + LEFT_PAD;
    let content_x = item_x + CHECKBOX_W + 1;

    for (row_i, (item_i, seg)) in rows[scroll_row..scroll_row + shown].iter().enumerate() {
        if line_y >= bottom_edge {
            break;
        }
        let todo = &todos[*item_i];
        // Checkbox only on the item's FIRST visible row; continuation rows
        // keep an aligned blank gutter so wrapped text stays readable.
        let first_visual = scroll_row + row_i == 0 || rows[scroll_row + row_i - 1].0 != *item_i;
        if first_visual {
            let (symbol, fg_color) = match todo.status.as_str() {
                "in_progress" => ("\u{2022}", theme.warning),
                "completed" => ("\u{2713}", theme.text_muted),
                _ => (" ", theme.text_muted),
            };
            let checkbox = format!("[{symbol}]");
            draw_text(
                buf,
                &checkbox,
                item_x,
                line_y,
                CHECKBOX_W,
                Style::default().fg(rgba_color(fg_color)),
            );
        } else {
            draw_text(buf, "   ", item_x, line_y, CHECKBOX_W, Style::default());
        }

        if content_max_w > 0 {
            let fg = if todo.status == "in_progress" {
                theme.warning
            } else {
                theme.text_muted
            };
            let mut style = Style::default().fg(rgba_color(fg));
            let mut seg = seg.as_str();
            if todo.status == "completed" {
                // Completed items wipe left-to-right: the already-swept prefix
                // carries the strikethrough modifier, the rest stays plain.
                // The reveal count is per ITEM (not per row): each wrapped
                // continuation row reveals once the sweep has passed the
                // characters the earlier rows consumed. `None` = animation
                // over (or never ran for this item): full permanent strike.
                let reveal = strike_frames
                    .get(*item_i)
                    .copied()
                    .flatten()
                    .and_then(|frame| strike_reveal_count(&todo.content, frame));
                if let Some(reveal) = reveal {
                    let consumed: usize = rows[scroll_row..scroll_row + row_i]
                        .iter()
                        .filter(|(prev_i, _)| prev_i == item_i)
                        .map(|(_, prev_seg)| prev_seg.chars().count())
                        .sum();
                    let struck_here = reveal.saturating_sub(consumed);
                    let seg_chars = seg.chars().count();
                    let tail = seg_chars.saturating_sub(struck_here);
                    if tail == 0 {
                        style = style.add_modifier(Modifier::CROSSED_OUT);
                    } else if struck_here > 0 {
                        let split = seg
                            .char_indices()
                            .nth(seg_chars - tail)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let (struck, rest) = seg.split_at(split);
                        let struck_style = style.add_modifier(Modifier::CROSSED_OUT);
                        draw_text(buf, struck, content_x, line_y, content_max_w, struck_style);
                        let struck_w = struck.chars().count() as u16;
                        seg = rest;
                        draw_text(
                            buf,
                            seg,
                            content_x + struck_w,
                            line_y,
                            content_max_w.saturating_sub(struck_w),
                            style,
                        );
                        line_y += 1;
                        continue;
                    }
                } else {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
            }
            draw_text(buf, seg, content_x, line_y, content_max_w, style);
        }

        line_y += 1;
    }

    total_used
}

/// Estimate the height needed for the todo section: every item contributes
/// its WRAPPED visual row count at the section's content width (mirrors
/// [`render_todo_section`] exactly so the box never clips or overhangs).
pub fn todo_section_height(todos: &[TodoItem], max_w: u16) -> u16 {
    if todos.is_empty() {
        return 0;
    }
    let content_max_w = content_width(max_w);
    // Cap at 30 items to avoid extreme natural_h values; mirrors the
    // renderer's 5000-row safety valve so height can never exceed paint.
    let mut rows: u16 = 0;
    for t in todos.iter().take(30) {
        rows = rows.saturating_add(wrap_chars(&t.content, content_max_w).len() as u16);
        if rows > 5000 {
            rows = 5000;
            break;
        }
    }
    TOP_GAP + TOP_PAD + 1 + rows + BOTTOM_PAD
}

/// Simple text drawing helper. Uses `checked_add` to avoid u16 overflow
/// when computing character positions.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn test_theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    /// Long todo contents WRAP onto aligned continuation rows instead of
    /// being cut with an ellipsis; the checkbox appears only on the first
    /// row of the item.
    #[test]
    fn long_todo_content_wraps_without_ellipsis() {
        let theme = test_theme();
        let todos = vec![TodoItem {
            status: "in_progress".to_string(),
            content:
                "run cargo build --bin cosh --features embed and then run the full test suite twice"
                    .to_string(),
        }];
        let max_w = 42u16; // RIGHT_PANEL_WIDTH
        let h = todo_section_height(&todos, max_w);
        // content width 37 → 88 chars / 37 ≈ 3 visual rows.
        assert!(
            h > TOP_GAP + TOP_PAD + 1 + 2 + BOTTOM_PAD,
            "wrapped height {h}"
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, max_w, h + TOP_GAP));
        render_todo_section(&mut buf, 0, 0, max_w, h, &todos, &[], &theme, None);
        let all: String = (0..h + TOP_GAP)
            .map(|y| {
                (0..max_w)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().chars().next().unwrap_or(' '))
                            .unwrap_or(' ')
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains('\u{2026}'), "ellipsis must be gone: {all:?}");
        assert!(
            all.contains("cargo build") && all.contains("twice"),
            "wrapped segments must cover the whole content: {all:?}"
        );
        // Checkbox exactly once.
        assert_eq!(all.matches("[\u{2022}]").count(), 1);
    }

    /// Height mirrors the renderer's flattened row count for mixed items.
    #[test]
    fn todo_height_matches_wrapped_rows() {
        let todos = vec![
            TodoItem {
                status: "pending".into(),
                content: "short".into(),
            },
            TodoItem {
                status: "completed".into(),
                content: "w".repeat(80),
            },
        ];
        let h = todo_section_height(&todos, 42);
        let expected_rows = 1 + wrap_chars(&"w".repeat(80), content_width(42)).len() as u16;
        assert_eq!(h, TOP_GAP + TOP_PAD + 1 + expected_rows + BOTTOM_PAD);
    }
}
