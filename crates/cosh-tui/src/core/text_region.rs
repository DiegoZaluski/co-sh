//! Text-region extraction for copy-by-selection.
//!
//! A [`TextRegion`] is a single row of selectable text expressed in a hybrid
//! coordinate space: rows are *content* rows (absolute, scroll-independent)
//! while columns are *screen* columns. Builders sample rendered cells or
//! wrapped lines; the extractor slices regions by a drag band.
//!
//! # The content-only contract
//!
//! Region `text` must be **content-only**: `text` char 0 corresponds to
//! screen column `x1`, char 1 to `x1 + 1`, and so on. Builders that sample
//! full-width rows (message borders, margins, scrollbar gutters) MUST strip
//! those columns before labeling the region with the content `x1` — sampling
//! from column 0 while labeling `x1 = content_start` injects ghost leading
//! columns and shifts every copy three-plus characters off the painted
//! selection. The helpers in this module enforce that contract by taking the
//! content origin and the border width as parameters and doing the strip
//! internally, so call sites have no offset arithmetic left to get wrong.
//!
//! # The inclusive-focus contract
//!
//! [`extract_text_in_region`] treats the drag's focus column as INCLUSIVE,
//! mirroring what selection painters highlight (`lx1..=lx2`): the glyph the
//! user sees painted under the release point is part of the copy. This
//! includes the single-cell band of a drag that starts and ends on the same
//! cell, which copies exactly that one glyph.

use ratatui::buffer::Cell;

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

impl TextRegion {
    /// Build a one-row region (`y2 = y1 + 1`).
    ///
    /// `text` char 0 maps to screen column `x1`, char 1 to `x1 + 1`, and so
    /// on — the content-only contract. The text may legitimately be a little
    /// wider than the labeled span: builders that sample a window wider than
    /// the content width trim trailing blanks, but a gutter glyph in the last
    /// sampled column can survive; the extractor clamps at `x2`, so it is
    /// inert.
    pub fn one_row(y1: i32, x1: u16, x2: u16, text: String) -> Self {
        Self {
            y1,
            y2: y1 + 1,
            x1,
            x2,
            text,
        }
    }
}

/// Read the first `w` cells of a row slice into trimmed line text: one char
/// per cell (wide glyphs contribute their base cell; spacer cells carry
/// `' '`), trailing whitespace trimmed. The shared inner loop of every
/// cell-sampling builder.
pub fn text_from_cell_row(cells: &[Cell], w: usize) -> String {
    let mut line_text = String::with_capacity(w);
    for dx in 0..w {
        line_text.push(
            cells
                .get(dx)
                .and_then(|cell| cell.symbol().chars().next())
                .unwrap_or(' '),
        );
    }
    line_text.trim_end().to_string()
}

/// Sample rows out of a full-width cell grid into content-only regions.
///
/// `cells` is a row-major grid of `stride` columns; each row's first
/// `border_cols` columns (message border, margin, gutter) are STRIPPED so
/// the region text is content-only, and the region is labeled with the
/// content origin the caller renders at. `row_y` pairs each grid row index
/// with the content row it occupies — pass `(0..rows).zip(first_content_y..)`
/// for an unclipped grid, or a clipped pair for a straddling one.
///
/// This is the single place that knows the border-strip arithmetic. A row
/// index beyond the grid is a caller pairing bug: the walk stops (no panic,
/// matching `regions_from_wrapped_lines`), so a truncated grid truncates the
/// region list instead of taking the renderer down.
pub fn regions_from_full_width_cells(
    cells: &[Cell],
    stride: usize,
    border_cols: usize,
    x1: u16,
    x2: u16,
    row_y: impl Iterator<Item = (usize, i32)>,
) -> Vec<TextRegion> {
    let content_w = stride.saturating_sub(border_cols);
    let mut regions = Vec::new();
    for (row, y) in row_y {
        let base = row * stride;
        if base + border_cols >= cells.len() {
            break;
        }
        let text = text_from_cell_row(&cells[base + border_cols..], content_w);
        if text.is_empty() {
            continue;
        }
        regions.push(TextRegion::one_row(y, x1, x2, text));
    }
    regions
}

/// Pair up already-wrapped visual lines into content-only regions.
///
/// `lines` are visual rows produced by a wrap pass (`word_wrap`, a markdown
/// layout scan); `row_y` maps each line's index to the content row it
/// occupies, so callers keep control of vertical pairing (blank-line
/// advancement, off-screen skips) while the labeling contract stays here.
pub fn regions_from_wrapped_lines(
    lines: &[&str],
    row_y: impl Iterator<Item = (usize, i32)>,
    x1: u16,
    x2: u16,
) -> Vec<TextRegion> {
    let mut regions = Vec::new();
    for (line_idx, y) in row_y {
        let Some(text) = lines.get(line_idx) else {
            break;
        };
        let text = text.trim_end().to_string();
        if text.is_empty() {
            continue;
        }
        regions.push(TextRegion::one_row(y, x1, x2, text));
    }
    regions
}

/// Sample a flattened row-major cell grid (`w` columns, `h` rows) into one
/// region per row, labeled `x1 = x_off`, `x2 = x_off + text_max_w`. The grid
/// must already be content-only — pre-slice full-width grids with the
/// `border_cols` strip of [`regions_from_full_width_cells`] or an equivalent
/// `base + border_cols` offset before calling. Rows are pushed even when
/// empty so vertical pairing stays positional; the extractor skips them.
pub fn cells_to_text_regions(
    cells: &[Cell],
    w: usize,
    h: u16,
    content_start_y: i32,
    x_off: u16,
    text_max_w: u16,
) -> Vec<TextRegion> {
    let mut regions = Vec::with_capacity(h as usize);
    for dy in 0..h as usize {
        let text = text_from_cell_row(&cells[dy * w..], w);
        regions.push(TextRegion::one_row(
            content_start_y + dy as i32,
            x_off,
            x_off + text_max_w,
            text,
        ));
    }
    regions
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
/// (`lx1..=lx2`). A same-cell band is a valid selection too — it copies
/// exactly that one glyph. Distinguishing a drag from a bare click is the
/// event layer's job (mouse-down followed by drag events vs. a plain
/// down+up release); this extractor never second-guesses geometry.
pub fn extract_text_in_region(
    regions: &[TextRegion],
    start_content_y: i32,
    end_content_y: i32,
    start_x: u16,
    end_x: u16,
) -> String {
    let mut result = String::new();
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
        // `lx2` is the drag's focus column. Selection painters highlight
        // `lx1..=lx2` INCLUSIVELY, so the character the user sees painted
        // under the release point must be copied too. The old exclusive slice
        // dropped it — releasing on the last glyph of a word copied the word
        // minus its final character. Full-width rows (middle rows and the top
        // row) are unaffected: lx2 = region.x2 there, and the +1 clamps back
        // to region.x2.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str, w: usize) -> Vec<Cell> {
        let mut cells: Vec<Cell> = s
            .chars()
            .map(|ch| {
                let mut c = Cell::default();
                c.set_char(ch);
                c
            })
            .collect();
        cells.resize(w, Cell::default());
        for c in &mut cells {
            if c.symbol() == "" {
                c.set_char(' ');
            }
        }
        cells
    }

    #[test]
    fn text_from_cell_row_trims_trailing_blanks() {
        assert_eq!(text_from_cell_row(&row("ab  ", 4), 4), "ab");
    }

    #[test]
    fn regions_from_full_width_cells_strips_border_columns() {
        // Full-width row: 3 border cols ("┃  ") then content "• hi".
        let mut cells = row("┃  • hi", 11);
        cells[7..].fill({
            let mut c = Cell::default();
            c.set_char(' ');
            c
        });

        let regions = regions_from_full_width_cells(&cells, 11, 3, 5, 5 + 8, (0..1).zip(0..1));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].x1, 5);
        assert_eq!(regions[0].text, "• hi");
        // Content-only: text char 0 maps to screen column x1. Same-row band
        // (0,0)-(6,0): inclusive focus column 6 pulls in the char at x1+1.
        assert_eq!(extract_text_in_region(&regions, 0, 0, 5, 6), "•");
    }

    #[test]
    fn regions_from_full_width_cells_skips_empty_rows() {
        let mut cells = row("┃  ", 11);
        cells[3..].fill({
            let mut c = Cell::default();
            c.set_char(' ');
            c
        });
        let regions = regions_from_full_width_cells(&cells, 11, 3, 5, 13, (0..1).zip(0..1));
        assert!(regions.is_empty());
    }

    #[test]
    fn regions_from_wrapped_lines_pairs_and_skips() {
        let lines = vec!["first", "", "third"];
        // Line index 1 is a blank wrap row: no region, next line advances.
        let regions = regions_from_wrapped_lines(&lines, (0..3).zip([10, 11, 12]), 3, 20);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].y1, 10);
        assert_eq!(regions[0].text, "first");
        assert_eq!(regions[1].y1, 12);
        assert_eq!(regions[1].text, "third");
    }

    #[test]
    fn cells_to_text_regions_labels_one_region_per_row() {
        // Two flattened content-only rows of 4 columns each.
        let mut cells = row("ab c", 4);
        cells.extend(row("xy  ", 4));
        let regions = cells_to_text_regions(&cells, 4, 2, 7, 3, 4);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].y1, 7);
        assert_eq!(regions[0].y2, 8);
        assert_eq!(regions[0].x1, 3);
        assert_eq!(regions[0].x2, 7);
        assert_eq!(regions[0].text, "ab c");
        assert_eq!(regions[1].text, "xy");
    }

    #[test]
    fn regions_from_full_width_cells_truncated_grid_stops() {
        // Grid holds ONE full row (11 cells) but the caller asks for two:
        // the walk must stop, not panic.
        let cells = row("┃  • hi     ", 11);
        let regions = regions_from_full_width_cells(&cells, 11, 3, 5, 13, (0..2).zip([0, 1]));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].y1, 0);
        assert_eq!(regions[0].text, "• hi");
    }

    #[test]
    fn extract_clamps_overlong_text_at_x2() {
        // Text longer than the labeled span (gutter glyph in the last
        // sampled column): a full-span band copies only up to x2 - 1.
        let region = TextRegion::one_row(0, 0, 5, "hello world".to_string());
        assert_eq!(
            extract_text_in_region(std::slice::from_ref(&region), 0, 0, 0, 4),
            "hello"
        );
    }

    #[test]
    fn extract_inclusive_focus_and_click_behavior() {
        let region = TextRegion::one_row(0, 0, 10, "hello world".to_string());
        // Inclusive focus: releasing ON the 'o' (column 4) copies it too —
        // the contract that fixed the "...visible_ro" bug.
        assert_eq!(
            extract_text_in_region(std::slice::from_ref(&region), 0, 0, 0, 4),
            "hello"
        );
        // One column further is a space: trimmed, same result.
        assert_eq!(
            extract_text_in_region(std::slice::from_ref(&region), 0, 0, 0, 5),
            "hello"
        );
        // A same-cell band copies exactly that glyph: a drag that starts and
        // ends on one cell selects the single character under it (bare
        // clicks never reach the extractor — the event layer filters them).
        assert_eq!(extract_text_in_region(&[region], 0, 0, 3, 3), "l");
    }
}
