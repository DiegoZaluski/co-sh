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

    /// Character columns the title actually occupies on screen — the string
    /// clamped to [`SessionListLayout::max_text_w`]. The string's char count
    /// is NOT the visible width: the renderer truncates at `max_text_w`, so
    /// a long title displays only its first `max_text_w` chars. Control
    /// characters are skipped because the renderer never draws them (session
    /// titles derive from user message text and can carry `\n`/`\t`).
    fn visible_title_w(area: Rect, title: &str) -> u16 {
        title
            .chars()
            .filter(|ch| !ch.is_control())
            .count()
            .min(Self::max_text_w(area) as usize) as u16
    }

    /// X of the 🗑 glyph for an item titled `title`: the title is clamped to
    /// [`SessionListLayout::max_text_w`] first, exactly as the renderer draws
    /// it, so the hit-test and the glyph can never drift apart.
    pub fn trash_x(area: Rect, title: &str) -> u16 {
        area.x + Self::LEFT_PAD + Self::PREFIX_W + Self::visible_title_w(area, title) + Self::GAP_W
    }

    /// Exact columns the visible (truncated) title occupies on an item's row
    /// — from after the selection prefix to its last displayed char. `None`
    /// when nothing is displayed (empty title, or a panel too narrow for any
    /// title text).
    ///
    /// The switch hit-test must use THIS, never `mx >= text_x` nor the
    /// string's length: the left pad and the selection-prefix columns before
    /// the title, the gap and everything past the glyph are not the string —
    /// a click there must not enter the session. The span is clamped to
    /// `max_text_w`, exactly what the renderer displays.
    pub fn title_span(area: Rect, title: &str) -> Option<std::ops::Range<u16>> {
        let visible = Self::visible_title_w(area, title);
        (visible > 0).then(|| {
            let start = area.x + Self::LEFT_PAD + Self::PREFIX_W;
            start..start + visible
        })
    }

    /// Exact columns the 🗑 glyph occupies on an item's row — `trash_x` and
    /// the one after it. `None` when the glyph is not drawn (it would cross
    /// the panel's right edge; render skips it, so the hit-test must too).
    ///
    /// The delete hit-test must use THIS, never `mx >= trash_x`: the gap,
    /// the cleared cell and the right pad are not the glyph — a click there
    /// must not open the delete confirm (nor switch: only the visible title
    /// text does, see [`SessionListLayout::title_span`]).
    pub fn trash_span(area: Rect, title: &str) -> Option<std::ops::Range<u16>> {
        let start = Self::trash_x(area, title);
        let end = start + Self::TRASH_W;
        (end <= area.right()).then_some(start..end)
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
