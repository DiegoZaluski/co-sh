/// A single row of selectable text, in content space.
///
/// `y1`/`y2` are content row bounds (inclusive/exclusive) and `x1`/`x2` are
/// SCREEN column bounds. Content space survives scroll changes, so a
/// selection started on mouse-down can keep tracking the text even while
/// auto-scroll moves the viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRegion {
    /// First content row (inclusive).
    pub y1: i32,
    /// Last content row (exclusive).
    pub y2: i32,
    pub x1: u16,
    pub x2: u16,
    pub text: String,
}

/// Extract the text spanned by a rectangular selection in content space.
///
/// `start_content_y`/`end_content_y` are the min/max content rows of the
/// selection; `start_x`/`end_x` are the screen columns of its first and last
/// row (following the drag direction). Each row that intersects the band is
/// sliced by its x overlap and joined with newlines, mirroring the flow-based
/// selection used by the chat.
///
/// Column bounds are INCLUSIVE on both ends: the glyph under the focus cell
/// is part of the selection, matching what both highlight painters draw
/// (`lx1..=lx2`). A single-cell band (no drag movement) copies nothing.
///
pub fn extract_text_in_region(
    regions: &[TextRegion],
    start_content_y: i32,
    end_content_y: i32,
    start_x: u16,
    end_x: u16,
) -> String {
    let mut result = String::new();
    // A selection spanning a single cell is a plain click, not a drag —
    // copy nothing (a click must not trigger the copy toast).
    if start_content_y == end_content_y && start_x == end_x {
        return String::new();
    }
    for region in regions {
        if region.y1 > end_content_y || region.y2 <= start_content_y {
            continue;
        }

        let line_y = region.y1;
        let (lx1, lx2) = if start_content_y == end_content_y {
            (start_x.min(end_x), start_x.max(end_x))
        } else if line_y == start_content_y {
            (start_x, region.x2)
        } else if line_y == end_content_y {
            (region.x1, end_x)
        } else {
            (region.x1, region.x2)
        };

        let ox1 = region.x1.max(lx1);
        // `lx2` is the drag's focus column. Both selection painters (chat and
        // right panel) highlight `lx1..=lx2` INCLUSIVELY, so the character the
        // user sees painted under the release point must be copied too. The
        // old exclusive slice dropped it — releasing on the last glyph of a
        // word copied the word minus its final character. Full-width rows
        // (middle rows and the top row) are unaffected: lx2 = region.x2 there,
        // and the +1 clamps back to region.x2.
        let ox2 = region.x2.min(lx2.saturating_add(1));
        if ox1 >= ox2 {
            continue;
        }

        let col_start = (ox1 - region.x1) as usize;
        let col_end = (ox2 - region.x1) as usize;

        let chars: Vec<char> = region.text.chars().collect();
        let line_len = chars.len();
        if col_start >= line_len {
            continue;
        }
        let end = col_end.min(line_len);

        let sliced: String = chars[col_start..end].iter().collect();
        // Defensive: builders pre-trim region text, so this is a no-op today;
        // it guards the inclusive focus column against future builders that
        // preserve trailing padding. Per-row only — interior indentation is
        // untouched.
        let sliced = sliced.trim_end();
        if !sliced.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(sliced);
        }
    }
    result
}

/// Extract text regions from a slice of rendered cells (flattened row-major).
/// Each output region occupies one content row (y2 = y1 + 1).
pub fn cells_to_text_regions(
    cells: &[ratatui::buffer::Cell],
    w: usize,
    h: u16,
    content_start_y: i32,
    x_off: u16,
    text_max_w: u16,
) -> Vec<TextRegion> {
    let mut regions = Vec::with_capacity(h as usize);
    for dy in 0..h as usize {
        let mut line_text = String::with_capacity(w);
        let base = dy * w;
        for dx in 0..w {
            line_text.push(cells[base + dx].symbol().chars().next().unwrap_or(' '));
        }
        let trimmed = line_text.trim_end().to_string();
        let cy = content_start_y + dy as i32;
        regions.push(TextRegion {
            y1: cy,
            y2: cy + 1,
            x1: x_off,
            x2: x_off + text_max_w,
            text: trimmed,
        });
    }
    regions
}
