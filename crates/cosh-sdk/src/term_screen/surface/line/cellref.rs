use crate::term_screen::cell::{Cell, CellAttributes};
use crate::term_screen::char_props::emoji::Presentation;
use core::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy)]
pub enum CellRef<'a> {
    CellRef {
        cell_index: usize,
        cell: &'a Cell,
    },
    ClusterRef {
        cell_index: usize,
        text: &'a str,
        width: usize,
        attrs: &'a CellAttributes,
    },
}

impl CellRef<'_> {
    #[must_use]
    pub fn cell_index(&self) -> usize {
        match self {
            Self::ClusterRef { cell_index, .. } | Self::CellRef { cell_index, .. } => *cell_index,
        }
    }

    #[must_use]
    pub fn str(&self) -> &str {
        match self {
            Self::CellRef { cell, .. } => cell.str(),
            Self::ClusterRef { text, .. } => text,
        }
    }

    #[must_use]
    pub fn width(&self) -> usize {
        match self {
            Self::CellRef { cell, .. } => cell.width(),
            Self::ClusterRef { width, .. } => *width,
        }
    }

    #[must_use]
    pub fn attrs(&self) -> &CellAttributes {
        match self {
            Self::CellRef { cell, .. } => cell.attrs(),
            Self::ClusterRef { attrs, .. } => attrs,
        }
    }

    #[must_use]
    pub fn presentation(&self) -> Presentation {
        match self {
            Self::CellRef { cell, .. } => cell.presentation(),
            Self::ClusterRef { text, .. } => match Presentation::for_grapheme(text) {
                (_, Some(variation)) => variation,
                (presentation, None) => presentation,
            },
        }
    }

    #[must_use]
    pub fn as_cell(&self) -> Cell {
        match self {
            Self::CellRef { cell, .. } => (*cell).clone(),
            Self::ClusterRef {
                text, width, attrs, ..
            } => Cell::new_grapheme_with_width(text, *width, (*attrs).clone()),
        }
    }

    #[must_use]
    pub fn same_contents(&self, other: &Self) -> bool {
        self.str() == other.str() && self.width() == other.width() && self.attrs() == other.attrs()
    }

    pub fn compute_shape_hash<H: Hasher>(&self, hasher: &mut H) {
        self.str().hash(hasher);
        self.attrs().compute_shape_hash(hasher);
    }
}
