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

#[test]
fn trash_span_covers_exactly_the_glyph_columns() {
    // "hi" -> trash_x = 7; the glyph occupies columns 7 and 8 (TRASH_W = 2).
    assert_eq!(SessionListLayout::trash_span(AREA, "hi"), Some(7..9));
    // Clamped long title -> trash_x = 17; columns 17 and 18.
    let long = "x".repeat(40);
    assert_eq!(SessionListLayout::trash_span(AREA, &long), Some(17..19));
}

#[test]
fn trash_span_is_none_when_the_glyph_would_cross_the_panel_edge() {
    // max_text_w = 0 -> trash_x = 5, glyph needs 5..7 but the panel is 6
    // wide: render skips the glyph, so the hit-test must not accept it.
    let tiny = Rect::new(0, 0, 6, 3);
    assert_eq!(SessionListLayout::trash_span(tiny, "anything"), None);
    // Same geometry, one column wider: end == right edge, still drawn.
    let exact = Rect::new(0, 0, 7, 3);
    assert_eq!(SessionListLayout::trash_span(exact, "ab"), Some(5..7));
}

#[test]
fn title_span_covers_exactly_the_visible_characters() {
    // Short title: prefix ends at x = LEFT_PAD(2) + PREFIX_W(2) = 4, and
    // "hi" displays both chars -> columns 4..6.
    assert_eq!(SessionListLayout::title_span(AREA, "hi"), Some(4..6));
    // A title exactly as wide as the budget spans the full text column.
    let title12 = "x".repeat(12);
    assert_eq!(SessionListLayout::title_span(AREA, &title12), Some(4..16));
}

#[test]
fn title_span_clamps_to_what_the_renderer_displays_not_the_string_length() {
    // A 40-char title in a 22-wide panel: only the first max_text_w = 12
    // chars are on screen, so the span is 4..16 — NOT 4..44. This is the
    // contract the switch hit-test relies on: the string's char count is
    // meaningless once the panel truncates it.
    let long = "x".repeat(40);
    assert_eq!(SessionListLayout::title_span(AREA, &long), Some(4..16));
    // The clamp must match trash_x's clamp: the glyph sits exactly one gap
    // past the span's end.
    assert_eq!(
        SessionListLayout::trash_x(AREA, &long),
        SessionListLayout::title_span(AREA, &long).unwrap().end + 1
    );
}

#[test]
fn title_span_is_none_without_any_visible_text() {
    // Empty title: nothing displayed, nothing clickable.
    assert_eq!(SessionListLayout::title_span(AREA, ""), None);
    // A panel too narrow for any title text (max_text_w = 0).
    let tiny = Rect::new(0, 0, 5, 3);
    assert_eq!(SessionListLayout::title_span(tiny, "anything"), None);
}

#[test]
fn control_characters_do_not_count_as_visible_width() {
    // The renderer skips control chars (titles derive from user message
    // text): "a\nb" draws 2 cells, so the span is 4..6 — not 4..7 — and the
    // glyph sits one column left of what an unfiltered count would claim.
    assert_eq!(SessionListLayout::title_span(AREA, "a\nb"), Some(4..6));
    assert_eq!(
        SessionListLayout::trash_x(AREA, "a\nb"),
        SessionListLayout::trash_x(AREA, "ab")
    );
}

#[test]
fn footer_span_covers_exactly_the_button_label() {
    // AREA (y 10..30, width 22): footer row = 29; the 10-char label is
    // centered — (22 - 10) / 2 = 6 -> columns 6..16.
    assert_eq!(SessionListLayout::footer_row(AREA), 29);
    assert_eq!(SessionListLayout::footer_span(AREA), Some(6..16));
}

#[test]
fn footer_span_is_none_when_the_label_or_row_does_not_fit() {
    // Panel narrower than the label: the button is not drawn, so the
    // hit-test must not accept clicks in its phantom columns.
    let narrow = Rect::new(0, 0, 9, 10);
    assert_eq!(SessionListLayout::footer_span(narrow), None);
    // Exactly wide enough: flush at the left edge.
    let exact = Rect::new(0, 0, 10, 10);
    assert_eq!(SessionListLayout::footer_span(exact), Some(0..10));
    // Panel too short: the footer row would sit on the header — not drawn.
    let short = Rect::new(0, 0, 22, 2);
    assert_eq!(SessionListLayout::footer_span(short), None);
}

#[test]
fn item_index_at_excludes_the_reserved_footer_row() {
    // AREA: items 12..29 (footer row 29 is the button's). Last item row is
    // 28; the footer row itself maps to no item.
    assert_eq!(SessionListLayout::item_index_at(AREA, 0, 28), Some(16));
    assert_eq!(SessionListLayout::item_index_at(AREA, 0, 29), None);
}
