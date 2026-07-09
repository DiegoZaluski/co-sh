#![allow(clippy::needless_pass_by_value)]
use super::cellref::CellRef;
use crate::term_screen::cell::Cell;
use serde::{Deserialize, Serialize};

use std::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VecStorage {
    cells: Vec<Cell>,
}

impl VecStorage {
    pub(crate) const fn new(cells: Vec<Cell>) -> Self {
        Self { cells }
    }

    pub(crate) fn set_cell(&mut self, idx: usize, cell: Cell, _clear_image_placement: bool) {
        self.cells[idx] = cell;
    }
}

impl core::ops::Deref for VecStorage {
    type Target = Vec<Cell>;

    fn deref(&self) -> &Vec<Cell> {
        &self.cells
    }
}

impl core::ops::DerefMut for VecStorage {
    fn deref_mut(&mut self) -> &mut Vec<Cell> {
        &mut self.cells
    }
}

/// Iterates over a slice of Cell, yielding only visible cells
pub struct VecStorageIter<'a> {
    pub cells: core::slice::Iter<'a, Cell>,
    pub idx: usize,
    pub skip_width: usize,
}

impl<'a> Iterator for VecStorageIter<'a> {
    type Item = CellRef<'a>;

    fn next(&mut self) -> Option<CellRef<'a>> {
        while self.skip_width > 0 {
            self.skip_width -= 1;
            let _ = self.cells.next()?;
            self.idx += 1;
        }
        let cell = self.cells.next()?;
        let cell_index = self.idx;
        self.idx += 1;
        self.skip_width = cell.width().saturating_sub(1);
        Some(CellRef::CellRef { cell_index, cell })
    }
}
