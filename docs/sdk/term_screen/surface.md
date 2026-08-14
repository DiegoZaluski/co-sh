# `term_screen::surface` — the display model

The contents of the screen: [`Line`]s made of [`Cell`]s with
[`CellAttributes`]. (Ported from WezTerm — see the [module overview](term_screen.md).)

## `Line` — one row of the screen

```rust,ignore
pub struct Line { /* cells + metadata: width, seqno, shape hash, bidi info */ }

pub fn new(seqno: SequenceNo) -> Self
pub fn with_width_and_cell(width: usize, cell: Cell, seqno: SequenceNo) -> Self
pub fn with_width(width: usize, seqno: SequenceNo) -> Self
pub fn from_cells(cells: Vec<Cell>, seqno: SequenceNo) -> Self
pub fn from_text(s: &str, attrs: &CellAttributes, seqno: SequenceNo, unicode_version: Option<&UnicodeVersion>) -> Self
pub fn from_text_with_wrapped_last_col(s: &str, attrs: &CellAttributes, seqno: SequenceNo) -> Self

pub fn resize(&mut self, width: usize, seqno: SequenceNo)
pub fn resize_and_clear(&mut self, width: usize, seqno: SequenceNo)
pub fn wrap(self, width: usize, seqno: SequenceNo) -> Vec<Self>   // split into wrapped lines

pub fn as_str(&self) -> Cow<'_, str>          // the line's text
pub fn split_off(&mut self, idx: usize, seqno: SequenceNo) -> Self

// Cell access:
//   line.get_cell(idx) -> Option<CellRef>   (None when the line is blank there)
//   line.visible_cells() -> iterator over CellRef
//   line.cells_mut() -> &mut [Cell]
//   line.len()
```

`SequenceNo` is a monotonically increasing counter (a `usize` type alias
re-exported through the `core` module) used for change tracking: mutations
bump the sequence number, and consumers compare per-line sequence numbers to
know what changed since their last render.

### Line metadata

```rust,ignore
pub fn compute_shape_hash(&self) -> [u8; 16]
pub fn is_single_width(&self) -> bool / set_single_width(&mut self, seqno)
pub fn is_double_width(&self) -> bool / set_double_width(&mut self, seqno)
pub fn is_double_height_top(&self) -> bool / set_double_height_bottom(&mut self, seqno)
pub fn has_hyperlink(&self) -> bool
pub fn semantic_zone_ranges(&mut self) -> &[ZoneRange]
pub fn update_last_change_seqno(&mut self, seqno: SequenceNo)
```

## `Cell` — one character position

```rust,ignore
pub struct Cell { /* text (TeenyString) + CellAttributes */ }

pub fn new(text: char, attrs: CellAttributes) -> Self
   // control/movement characters are rewritten as a space
impl Default for Cell { /* blank() */ }
pub const fn blank() -> Self
pub const fn blank_with_attrs(attrs: CellAttributes) -> Self

// accessors:
pub fn str(&self) -> &str            // the cell's text
pub fn width(&self) -> usize         // 1 or 2 (wide/combining)
pub const fn attrs(&self) -> &CellAttributes
pub const fn attrs_mut(&mut self) -> &mut CellAttributes
```

Attribute access is through `attrs()` / `attrs_mut()`, then the
`CellAttributes` getters (`foreground()`, `background()`, `intensity()`, ...).

## `CellAttributes` — styling

A bitfield struct — attributes are read with getters and set with **mutating
setters** that return `&mut Self`:

```rust,ignore
pub struct CellAttributes { /* bitfields */ }

pub const fn blank() -> Self            // Default::default(): plain

// getters:
pub fn foreground(&self) -> ColorAttribute
pub fn background(&self) -> ColorAttribute
pub fn intensity(&self) -> Intensity
pub fn underline(&self) -> Underline
pub fn blink(&self) -> Blink
pub fn italic(&self) -> bool
pub fn reverse(&self) -> bool
// ... and many more (strikethrough, hyperlink, font, ...)

// mutating setters (return &mut Self):
pub fn set_foreground<C: Into<ColorAttribute>>(&mut self, c: C) -> &mut Self
pub fn set_background<C: Into<ColorAttribute>>(&mut self, c: C) -> &mut Self
pub fn set_intensity(&mut self, i: Intensity) -> &mut Self
pub fn set_underline(&mut self, u: Underline) -> &mut Self
pub fn set_italic(&mut self, v: bool) -> &mut Self
pub fn set_reverse(&mut self, v: bool) -> &mut Self
// ...
```

Colors use `ColorAttribute` (re-exported from `core::color`):
`ColorAttribute::Default`, `ColorAttribute::PaletteIndex(u8)`, and the
TrueColor variants. `Intensity`/`Underline`/`Blink` are `Normal`/`Bold`/`Half`
etc. from the `csi` module.

## Building lines for your own rendering

A minimal way to construct a screen line by hand:

```rust,ignore
use cosh_sdk::term_screen::cell::{CellAttributes, Intensity};
use cosh_sdk::term_screen::core::{Line, color::ColorAttribute};

let mut attrs = CellAttributes::blank();
attrs
    .set_intensity(Intensity::Bold)
    .set_foreground(ColorAttribute::PaletteIndex(1));  // ANSI red
let line = Line::from_text("Hello", &attrs, 1, None);   // seqno, unicode version
assert_eq!(line.as_str(), "Hello");
assert_eq!(line.len(), 5);
```

---

Next: [escape_parser — parsing bytes into actions](escape_parser.md).
