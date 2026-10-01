use ratatui::buffer::Buffer;
use ratatui::style::Style;

/// Write `text` into `buf` starting at (`x`, `y`), clipped to `max_w` cells.
///
/// Control characters are skipped: writing them into buffer cells makes
/// ratatui's buffer diff panic ("control character passed to cell_width
/// without filtering"), and every text this renders ultimately derives from
/// model output, session titles or user input that can carry `\n`, `\t`,
/// `\r` or raw ANSI bytes.
///
/// Coordinate math is overflow-checked: if `x + max_w` (or `x + i`) would
/// wrap past `u16::MAX` the remaining text is clipped, mirroring the
/// majority of the originals this replaced.
pub fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
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

/// Compact variant of [`draw_text_line`]: control characters are dropped
/// BEFORE column assignment (`filter().enumerate()`), so unlike
/// [`draw_text_line`] they leave no gap — `"\nABC"` starts `ABC` at `x`.
/// This is the dialog/slash-menu semantics: their labels come from
/// curated/static strings where a gap would be a visible artifact.
pub fn draw_text_line_compact(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    max_w: u16,
    style: Style,
) {
    let right = x + max_w;
    for (i, ch) in text.chars().filter(|c| !c.is_control()).enumerate() {
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

/// Wide-glyph-aware variant of [`draw_text_line`]: CJK / emoji characters
/// occupy two terminal cells, so advancing one cell per char would shift
/// right-anchored segments and overwrite the wide char's second half. Used
/// for footer rows whose text is expected to contain wide glyphs.
pub fn draw_text_line_wide(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    let mut cx = x;
    for ch in text.chars() {
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

/// Fill a rectangle with spaces and one style.
pub fn fill_rect(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, style: Style) {
    for dy in 0..h {
        let row = y + dy;
        for dx in 0..w {
            if let Some(cell) = buf.cell_mut((x + dx, row)) {
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
    }
}

/// Draw a section title in `fg` on `bg` at (`x`, `y`).
pub fn section_title(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    title: &str,
    bg: ratatui::style::Color,
    fg: ratatui::style::Color,
) {
    for (i, ch) in title.chars().enumerate() {
        let cx = x + i as u16;
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(fg).bg(bg));
        }
    }
}
