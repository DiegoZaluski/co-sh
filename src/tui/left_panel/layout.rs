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
#[path = "test/layout.rs"]
mod tests;
