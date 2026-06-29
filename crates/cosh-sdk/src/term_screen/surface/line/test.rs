#![cfg(test)]

use super::clusterline::ClusteredLine;
use super::*;
use crate::term_screen::cell::{Cell, CellAttributes};
use crate::term_screen::surface::SEQ_ZERO;

/// There are 4 double-wide graphemes that occupy 2 cells each.
/// When we join the lines, we must preserve the invisible blank
/// that is part of the grapheme otherwise our metrics will be
/// wrong.
/// <https://github.com/wezterm/wezterm/issues/2568>
#[test]
fn append_line() {
    let mut line1: Line = "0123456789".into();
    let line2: Line = "グループaa".into();

    line1.append_line(line2, SEQ_ZERO);

    assert_eq!(line1.len(), 20);
}

#[test]
fn double_click_range_bounds() {
    let line: Line = "hello".into();
    let r = line.compute_double_click_range(200, |_| true);
    assert_eq!(r, DoubleClickRange::Range(200..200));
}

#[test]
fn cluster_representation_basic() {
    let line: Line = "hello".into();
    let mut compressed = line.clone();
    compressed.compress_for_scrollback();
    insta::assert_debug_snapshot!(&compressed.cells);

    compressed.coerce_vec_storage();
    assert_eq!(line, compressed);
}

#[test]
fn cluster_representation_double_width() {
    let line: Line = "❤ 😍🤢he❤ 😍🤢llo❤ 😍🤢".into();
    let mut compressed = line.clone();
    compressed.compress_for_scrollback();
    insta::assert_debug_snapshot!(&compressed.cells);

    compressed.coerce_vec_storage();
    assert_eq!(line, compressed);
}

#[test]
fn cluster_representation_empty() {
    let line = Line::from_cells(vec![], SEQ_ZERO);

    let mut compressed = line.clone();
    compressed.compress_for_scrollback();
    insta::assert_debug_snapshot!(&compressed.cells);

    compressed.coerce_vec_storage();
    assert_eq!(line, compressed);
}

#[test]
fn cluster_wrap_last() {
    let mut line: Line = "hello".into();
    line.compress_for_scrollback();
    line.set_last_cell_was_wrapped(true, 1);
    insta::assert_debug_snapshot!(line);
}

fn bold() -> CellAttributes {
    use crate::term_screen::cell::Intensity;
    let mut attr = CellAttributes::default();
    attr.set_intensity(Intensity::Bold);
    attr
}

#[test]
fn cluster_representation_attributes() {
    let line = Line::from_cells(
        vec![
            Cell::new_grapheme("a", CellAttributes::default(), None),
            Cell::new_grapheme("b", bold(), None),
            Cell::new_grapheme("c", CellAttributes::default(), None),
            Cell::new_grapheme("d", bold(), None),
        ],
        SEQ_ZERO,
    );

    let mut compressed = line.clone();
    compressed.compress_for_scrollback();
    insta::assert_debug_snapshot!(&compressed.cells);

    compressed.coerce_vec_storage();
    assert_eq!(line, compressed);
}

#[test]
fn cluster_append() {
    let mut cl = ClusteredLine::new();
    cl.append(Cell::new_grapheme("h", CellAttributes::default(), None));
    cl.append(Cell::new_grapheme("e", CellAttributes::default(), None));
    cl.append(Cell::new_grapheme("l", bold(), None));
    cl.append(Cell::new_grapheme("l", CellAttributes::default(), None));
    cl.append(Cell::new_grapheme("o", CellAttributes::default(), None));
    insta::assert_debug_snapshot!(cl);
}

#[test]
fn cluster_line_new() {
    let mut line = Line::new(1);
    line.set_cell(
        0,
        Cell::new_grapheme("h", CellAttributes::default(), None),
        1,
    );
    line.set_cell(
        1,
        Cell::new_grapheme("e", CellAttributes::default(), None),
        2,
    );
    line.set_cell(2, Cell::new_grapheme("l", bold(), None), 3);
    line.set_cell(
        3,
        Cell::new_grapheme("l", CellAttributes::default(), None),
        4,
    );
    line.set_cell(
        4,
        Cell::new_grapheme("o", CellAttributes::default(), None),
        5,
    );
    insta::assert_debug_snapshot!(line);
}
