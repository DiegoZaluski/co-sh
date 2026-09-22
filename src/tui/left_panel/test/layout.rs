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
