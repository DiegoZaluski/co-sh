# `text_region` — copy-by-selection text extraction

The `text_region` module provides types and functions for extracting text from selections in a terminal UI. It handles the conversion between screen coordinates and content coordinates, enabling copy-by-selection functionality that works correctly even when the viewport scrolls.

---

## The coordinate space

A `TextRegion` uses a **hybrid coordinate space**:

- **Rows** (`y1`, `y2`) are *content rows* — absolute, scroll-independent positions in the document.
- **Columns** (`x1`, `x2`) are *screen columns* — positions on the terminal display.

This separation allows a selection to track the text correctly even when auto-scroll moves the viewport. The selection is defined in content space (so it points to the right text) but rendered in screen space (so it paints under the mouse).

---

## `TextRegion`

A single row of selectable text.

```rust
pub struct TextRegion {
    pub y1: i32,           // First content row (inclusive)
    pub y2: i32,           // Last content row (exclusive)
    pub x1: u16,           // Screen column start (inclusive)
    pub x2: u16,           // Screen column end (exclusive)
    pub text: String,      // The text content
}
```

### The content-only contract

The `text` field must be **content-only**: character 0 corresponds to screen column `x1`, character 1 to `x1 + 1`, and so on. This contract ensures that when you slice the text by screen coordinates, you get the correct characters.

Builders that sample full-width rows (including borders, margins, scrollbar gutters) **must strip** those columns before labeling the region with the content `x1`. If you sample from column 0 but label `x1 = content_start`, you inject ghost leading columns and the copy will be shifted relative to the painted selection.

The helpers in this module enforce this contract by taking the content origin and border width as parameters and doing the strip internally.

### Construction

```rust
pub fn one_row(y1: i32, x1: u16, x2: u16, text: String) -> Self
```

Builds a one-row region (`y2 = y1 + 1`). The text may legitimately be wider than the labeled span (builders trim trailing blanks, but a gutter glyph in the last sampled column can survive). The extractor clamps at `x2`, so extra text is inert.

---

## Builders

### `text_from_cell_row`

Reads the first `w` cells of a row slice into trimmed line text.

```rust
pub fn text_from_cell_row(cells: &[Cell], w: usize) -> String
```

- Extracts one character per cell (wide glyphs contribute their base cell; spacer cells carry `' '`).
- Trims trailing whitespace.
- This is the shared inner loop of every cell-sampling builder.

### `regions_from_full_width_cells`

Samples rows from a full-width cell grid into content-only regions.

```rust
pub fn regions_from_full_width_cells(
    cells: &[Cell],
    stride: usize,
    border_cols: usize,
    x1: u16,
    x2: u16,
    row_y: impl Iterator<Item = (usize, i32)>,
) -> Vec<TextRegion>
```

- `cells` is a row-major grid of `stride` columns.
- Each row's first `border_cols` columns (message border, margin, gutter) are **stripped** so the region text is content-only.
- The region is labeled with the content origin the caller renders at.
- `row_y` pairs each grid row index with the content row it occupies (pass `(0..rows).zip(first_content_y..)` for an unclipped grid).
- This is the single place that knows the border-strip arithmetic.
- A row index beyond the grid stops the walk (no panic), so a truncated grid truncates the region list instead of crashing.

### `regions_from_wrapped_lines`

Pairs wrapped visual lines into content-only regions.

```rust
pub fn regions_from_wrapped_lines(
    lines: &[&str],
    row_y: impl Iterator<Item = (usize, i32)>,
    x1: u16,
    x2: u16,
) -> Vec<TextRegion>
```

- `lines` are visual rows produced by a wrap pass (`word_wrap`, markdown layout scan).
- `row_y` maps each line's index to the content row it occupies.
- Blank lines are skipped (no region is created for them).
- Allows callers to control vertical pairing (blank-line advancement, off-screen skips) while the labeling contract stays here.

### `cells_to_text_regions`

Samples a flattened row-major cell grid into one region per row.

```rust
pub fn cells_to_text_regions(
    cells: &[Cell],
    w: usize,
    h: u16,
    content_start_y: i32,
    x_off: u16,
    text_max_w: u16,
) -> Vec<TextRegion>
```

- The grid must already be **content-only** (pre-slice full-width grids with the `border_cols` strip of `regions_from_full_width_cells` before calling).
- Each region is labeled `x1 = x_off`, `x2 = x_off + text_max_w`.
- Rows are pushed even when empty so vertical pairing stays positional; the extractor skips them.

---

## Extraction

### `extract_text_in_region`

Extracts the text spanned by a rectangular selection in content space.

```rust
pub fn extract_text_in_region(
    regions: &[TextRegion],
    start_content_y: i32,
    end_content_y: i32,
    start_x: u16,
    end_x: u16,
) -> String
```

- `start_content_y`/`end_content_y` are the min/max content rows of the selection.
- `start_x`/`end_x` are the screen columns of its first and last row (following the drag direction).
- Each row that intersects the band is sliced by its x overlap and joined with newlines.
- Mirrors the flow-based selection used by the chat.

### The inclusive-focus contract

Column bounds are **inclusive on both ends**: the glyph under the focus cell is part of the selection, matching what highlight painters draw (`lx1..=lx2`).

This matters for single-cell selections and for releasing on the last character of a word. A same-cell band is a valid selection — it copies exactly that one glyph. Distinguishing a drag from a bare click is the event layer's job (mouse-down followed by drag events vs. a plain down+up release); this extractor never second-guesses geometry.

### Example

```rust,ignore
use cosh_tui::core::text_region::{TextRegion, extract_text_in_region};

let region = TextRegion::one_row(0, 0, 10, "hello world".to_string());

// Inclusive focus: releasing ON the 'o' (column 4) copies it too
let text = extract_text_in_region(&[region], 0, 0, 0, 4);
assert_eq!(text, "hello");

// One column further is a space: trimmed, same result
let text = extract_text_in_region(&[region], 0, 0, 0, 5);
assert_eq!(text, "hello");

// Same-cell band copies exactly that glyph
let text = extract_text_in_region(&[region], 0, 0, 3, 3);
assert_eq!(text, "l");
```

---

## The content-only contract in detail

### Why it matters

When a renderer draws a message with borders, margins, and scrollbars, the full screen row includes decorative columns that are not part of the content. If you sample from column 0 but label the region with `x1 = content_start`, the character indexing is misaligned:

```
Screen row: [border][margin][gutter][content starts here...]
Index:       0       1       2       3                ...

If you sample from 0 but label x1 = 3:
- text[0] corresponds to screen column 0 (border)
- But x1 = 3 claims text[0] corresponds to screen column 3 (content)
- Result: copy is shifted by 3 characters
```

The fix: strip the border columns before building the region text, then label `x1` with the content origin. The module's helpers do this automatically.

### Enforcement

`regions_from_full_width_cells` takes `border_cols` as a parameter and strips internally:

```rust,ignore
let regions = regions_from_full_width_cells(
    &cells,      // full-width grid
    stride,      // total columns per row
    border_cols, // columns to strip
    x1, x2,      // screen bounds (after strip)
    row_y,       // vertical pairing
);
```

Call sites provide the content origin they render at, and the helper ensures the text matches that origin.

---

## Summary

- `TextRegion` represents a single row of selectable text in hybrid coordinate space (content rows, screen columns).
- The content-only contract: region text must be stripped of non-content columns, and `x1` must be the content origin.
- Builders convert cell grids or wrapped lines into content-only regions: `text_from_cell_row`, `regions_from_full_width_cells`, `regions_from_wrapped_lines`, `cells_to_text_regions`.
- `extract_text_in_region` extracts text from a rectangular selection using inclusive focus (matching highlight painters).
- The inclusive-focus contract: the glyph under the focus cell is part of the selection, enabling correct single-cell selections and last-character copying.
- Coordinate separation (content rows, screen columns) allows selections to track text correctly even during auto-scroll.

Next: [types — attributes, mouse events, selection](types.md).
