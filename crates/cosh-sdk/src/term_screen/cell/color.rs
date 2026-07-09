//! Colors for attributes
// for FromPrimitive
#![allow(clippy::useless_attribute)]

pub use crate::term_screen::color_types::{LinearRgba, SrgbaTuple};
use serde::{Deserialize, Serialize};

pub use crate::term_screen::escape_parser::color::{AnsiColor, ColorSpec, PaletteIndex, RgbColor};

/// Specifies the color to be used when rendering a cell.  This is the
/// type used in the `CellAttributes` struct and can specify an optional
/// `TrueColor` value, allowing a fallback to a more traditional palette
/// index if `TrueColor` is not available.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Eq, PartialEq, Hash, Default)]
pub enum ColorAttribute {
    /// Use `RgbColor` when supported, falling back to the specified `PaletteIndex`.
    TrueColorWithPaletteFallback(SrgbaTuple, PaletteIndex),
    /// Use `RgbColor` when supported, falling back to the default color
    TrueColorWithDefaultFallback(SrgbaTuple),
    /// Use the specified `PaletteIndex`
    PaletteIndex(PaletteIndex),
    /// Use the default color
    #[default]
    Default,
}

impl From<AnsiColor> for ColorAttribute {
    fn from(col: AnsiColor) -> Self {
        Self::PaletteIndex(col as u8)
    }
}

impl From<ColorSpec> for ColorAttribute {
    fn from(spec: ColorSpec) -> Self {
        match spec {
            ColorSpec::Default => Self::Default,
            ColorSpec::PaletteIndex(idx) => Self::PaletteIndex(idx),
            ColorSpec::TrueColor(color) => Self::TrueColorWithDefaultFallback(color),
        }
    }
}
