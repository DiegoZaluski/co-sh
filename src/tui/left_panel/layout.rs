//! Single source of truth for the sessions-list geometry — the anti-mirror.
//!
//! Render (`sessions.rs`), mouse hit-testing (`SessionsView::handle_mouse`)
//! and the hover tooltip (`app/render.rs`) all derive their coordinates from
//! here. Never recompute `area.y + 2`, the 🗑 column or the title width
//! outside this module: a second copy of any of these expressions is the
//! sync hazard this module exists to kill.

use ratatui::layout::Rect;

/// Layout contract of the sessions list inside the left panel.
pub struct SessionListLayout;

impl SessionListLayout {
    /// `" Sessions"` header row + separator line.
    pub const HEADER_ROWS: u16 = 2;
    pub const LEFT_PAD: u16 = 2;
    /// Selection-indicator column pair before the title.
    pub const PREFIX_W: u16 = 2;
    /// Gap between the title and the 🗑 glyph.
    pub const GAP_W: u16 = 1;
    /// 🗑 emoji occupies 2 columns.
    pub const TRASH_W: u16 = 2;
    /// Cleared cell after the wide emoji.
    pub const CLEAR_W: u16 = 1;
    pub const RIGHT_PAD: u16 = 2;

    /// First row of list items (below header + separator).
    pub fn content_start_y(area: Rect) -> u16 {
        area.y + Self::HEADER_ROWS
    }

    /// Max character columns a session title may occupy.
    pub fn max_text_w(area: Rect) -> u16 {
        area.width.saturating_sub(
            Self::LEFT_PAD
                + Self::PREFIX_W
                + Self::GAP_W
                + Self::TRASH_W
                + Self::CLEAR_W
                + Self::RIGHT_PAD,
        )
    }

    /// X of the 🗑 glyph for an item titled `title`: the title is clamped to
    /// [`SessionListLayout::max_text_w`] first, exactly as the renderer draws
    /// it, so the hit-test and the glyph can never drift apart.
    pub fn trash_x(area: Rect, title: &str) -> u16 {
        let visible = title.chars().count().min(Self::max_text_w(area) as usize) as u16;
        area.x + Self::LEFT_PAD + Self::PREFIX_W + visible + Self::GAP_W
    }

    /// Index of the list item under row `y`, accounting for the scroll
    /// offset. `None` outside the list rows (header, separator, past bottom).
    pub fn item_index_at(area: Rect, scroll: usize, y: u16) -> Option<usize> {
        let start = Self::content_start_y(area);
        if y < start || y >= area.bottom() {
            return None;
        }
        Some(scroll + (y - start) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Panel-sized area anchored at y=10: header rows 10-11, items 12..30.
    const AREA: Rect = Rect::new(0, 10, 22, 20);

    #[test]
    fn max_text_w_is_width_minus_the_full_column_budget() {
        // LEFT_PAD 2 + PREFIX_W 2 + GAP_W 1 + TRASH_W 2 + CLEAR_W 1 + RIGHT_PAD 2 = 10
        assert_eq!(SessionListLayout::max_text_w(AREA), 12);
        assert_eq!(SessionListLayout::max_text_w(Rect::new(0, 0, 5, 3)), 0);
    }

    #[test]
    fn content_start_y_skips_the_header_rows() {
        assert_eq!(SessionListLayout::content_start_y(AREA), 12);
    }

    #[test]
    fn item_index_at_maps_rows_and_respects_bounds() {
        assert_eq!(SessionListLayout::item_index_at(AREA, 0, 11), None); // header
        assert_eq!(SessionListLayout::item_index_at(AREA, 0, 12), Some(0));
        assert_eq!(SessionListLayout::item_index_at(AREA, 0, 15), Some(3));
        assert_eq!(SessionListLayout::item_index_at(AREA, 7, 12), Some(7));
        assert_eq!(SessionListLayout::item_index_at(AREA, 0, 30), None); // past bottom
    }

    #[test]
    fn trash_x_sits_after_prefix_plus_visible_title_plus_gap() {
        // area.x(0) + LEFT_PAD(2) + PREFIX_W(2) + "hi"(2) + GAP_W(1)
        assert_eq!(SessionListLayout::trash_x(AREA, "hi"), 7);
        // Long titles clamp to max_text_w, same as the renderer's truncation:
        // 0 + 2 + 2 + 12 + 1
        let long = "x".repeat(40);
        assert_eq!(SessionListLayout::trash_x(AREA, &long), 17);
    }
}
