use super::*;

/// Copy `rows` full rows of `src_cells` (a `stride`-wide cell grid) into
/// `buf` starting at (`dst_x`, `dst_y`).
///
/// The render loop's hottest blits used to go through `buf.cell_mut((x, y))`
/// per cell — each call pays a position→index conversion plus bounds check.
/// `Buffer` stores cells contiguous per row, so copying one `Cell` slice per
/// row amortizes that to a single bounds check per row: the per-frame cost of
/// scrolling/streaming a large transcript drops accordingly (this is what
/// keeps the adaptive frame pacer in the smooth 60 fps tier on mid-range
/// Windows laptops).
///
/// Rows are skipped (not partially copied) when the destination row falls
/// outside `buf`'s area, matching the old per-cell `cell_mut(..) == None`
/// behavior; `count` cells are copied from the start of each source row.
#[allow(clippy::too_many_arguments)]
pub(super) fn blit_cell_rows(
    buf: &mut Buffer,
    dst_x: u16,
    dst_y: u16,
    src_cells: &[Cell],
    stride: usize,
    first_row: usize,
    rows: usize,
    count: usize,
) {
    let buf_w = usize::from(buf.area.width);
    let buf_h = usize::from(buf.area.height);
    if stride == 0 || buf_w == 0 {
        return;
    }
    let count = count.min(stride).min(buf_w);
    for row in 0..rows {
        let src_base = (first_row + row) * stride;
        let Some(src) = src_cells.get(src_base..src_base + count) else {
            break;
        };
        let dst_row = usize::from(dst_y) + row;
        if dst_row >= buf_h {
            break;
        }
        let dst_base = dst_row * buf_w + usize::from(dst_x);
        let Some(dst) = buf.content.get_mut(dst_base..dst_base + count) else {
            break;
        };
        dst.clone_from_slice(src);
    }
}

/// Draws the expand/collapse hint styled as a button: highlighted with the
/// theme primary color as a marker-like background so it reads as clickable.
pub(super) fn draw_hint_button(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, theme: &Theme) {
    let style = Style::default()
        .fg(rgba_color(theme.background))
        .bg(rgba_color(theme.primary));
    let width = (text.chars().count() as u16).saturating_add(2);
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    let right = right.min(x.saturating_add(width));
    for cx in x..right {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_style(style);
        }
    }
    let inner_x = x.saturating_add(1);
    let inner_max_w = max_w.saturating_sub(2);
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let Some(cx) = inner_x.checked_add(i as u16) else {
            break;
        };
        if cx >= inner_x.saturating_add(inner_max_w) {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub(super) const fn concealed_char(ch: char) -> char {
    if ch == ' ' { ' ' } else { '\u{2588}' }
}

pub(super) fn sanitize_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect()
}

pub(super) fn conceal_text(text: &str) -> String {
    text.chars().map(concealed_char).collect()
}

/// Hash message part content for cache invalidation.
///
/// Hashes the actual content bytes of each part (text body, tool name,
/// reasoning text, filename) using `DefaultHasher::write()`, not just
/// their lengths. This ensures that switching sessions with different
/// content produces different tokens, preventing stale cache hits.
///
/// Without content-aware hashing, two messages with same-length parts
/// would produce identical tokens, causing the per-message render
/// cache to serve stale cells from a different session.
pub(super) fn hash_parts(msg: &Message) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Hash part count
    hasher.write_usize(msg.parts.len());
    for part in &msg.parts {
        match part {
            Part::Text(t) => {
                // Hash actual content bytes, not just length
                hasher.write(t.text.as_bytes());
                hasher.write(&[t.synthetic as u8]);
            }
            Part::Tool(t) => {
                hasher.write(t.tool.as_bytes());
                hasher.write(&[status_to_u8(&t.status)]);
                if let Some(ref out) = t.output {
                    hasher.write(out.as_bytes());
                }
                if let Some(ref id) = t.tool_call_id {
                    hasher.write(id.as_bytes());
                }
                if let Some(notes) = &t.lsp_notes {
                    for note in notes.errors.iter().chain(notes.warnings.iter()) {
                        hasher.write(note.path.as_bytes());
                        hasher.write(&note.line.to_le_bytes());
                        hasher.write(note.message.as_bytes());
                    }
                }
            }
            Part::Reasoning(r) => {
                hasher.write(r.text.as_bytes());
            }
            Part::File(f) => {
                hasher.write(f.filename.as_bytes());
                hasher.write(f.mime.as_bytes());
            }
            Part::Compaction(c) => {
                // Hash the full state so the line re-renders exactly once when
                // it finalizes (elapsed_ms transitions None → Some), and so a
                // streamed "Summarizing" body (phase Llm text) invalidates the
                // cache as tokens arrive. While the stopwatch alone ticks, the
                // hash stays stable — the ticking bypasses the cell cache via
                // `msg_has_running_compaction`.
                hasher.write(&[0u8]);
                hasher.write(&c.started_at.to_le_bytes());
                hasher.write(c.text.as_bytes());
                match c.elapsed_ms {
                    Some(ms) => {
                        hasher.write(&[1]);
                        hasher.write(&ms.to_le_bytes());
                    }
                    None => hasher.write(&[0]),
                }
            }
            Part::Install(p) => {
                // Same rule as Compaction: hash the full state so progress
                // deltas re-render the line and the terminal event renders
                // exactly once more.
                hasher.write(&[2u8]);
                hasher.write(&p.bytes_done.to_le_bytes());
                hasher.write(&p.bytes_total.to_le_bytes());
                hasher.write(&p.started_at.to_le_bytes());
                match (p.elapsed_ms, p.failed) {
                    (Some(ms), failed) => {
                        hasher.write(&[1]);
                        hasher.write(&ms.to_le_bytes());
                        hasher.write(&[failed.unwrap_or(false) as u8]);
                    }
                    (None, _) => hasher.write(&[0]),
                }
            }
        }
    }
    hasher.finish()
}
