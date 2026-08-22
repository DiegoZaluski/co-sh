use ratatui::buffer::Cell;
use ratatui::style::Style;

/// A vertically-growable canvas of styled cells.
///
/// The block renderer writes into a `GrowBuf` instead of the terminal
/// `Buffer` so that a block's rendered rows can be measured (height) and
/// cached (phase 2) independently of the viewport. `cell_mut` mirrors
/// `Buffer::cell_mut` semantics: out-of-bounds columns return `None`, rows
/// grow on demand and start pre-filled with `base_style` (the area background)
/// so that blitting a cached row reproduces exactly what a direct render
/// would have painted.
pub(crate) struct GrowBuf {
    width: u16,
    blank: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
}

impl GrowBuf {
    pub(crate) fn new(width: u16, base_style: Style) -> Self {
        let mut blank = Vec::with_capacity(usize::from(width));
        for _ in 0..width {
            let mut cell = Cell::from(' ');
            cell.set_style(base_style);
            blank.push(cell);
        }
        Self {
            width,
            blank,
            rows: Vec::new(),
        }
    }

    /// Mutable access to the cell at `(x, y)`, growing vertically as needed.
    /// Returns `None` when `x` is outside the canvas width.
    #[allow(clippy::inline_always)]
    pub(crate) fn cell_mut(&mut self, (x, y): (u16, u16)) -> Option<&mut Cell> {
        if self.width == 0 || x >= self.width {
            return None;
        }
        while self.rows.len() <= usize::from(y) {
            self.rows.push(self.blank.clone());
        }
        Some(&mut self.rows[usize::from(y)][usize::from(x)])
    }

    /// Consume the canvas, returning the rendered rows.
    pub(crate) fn into_rows(self) -> Vec<Vec<Cell>> {
        self.rows
    }
}
