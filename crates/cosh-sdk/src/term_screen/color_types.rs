//! Types for working with colors
//! Inline replacement for wezterm-color-types crate

use core::fmt;
use serde::{Deserialize, Serialize};

/// An SRGB tuple with alpha
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct SrgbaTuple(pub f64, pub f64, pub f64, pub f64);

impl PartialEq for SrgbaTuple {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
            && self.1.to_bits() == other.1.to_bits()
            && self.2.to_bits() == other.2.to_bits()
            && self.3.to_bits() == other.3.to_bits()
    }
}
impl Eq for SrgbaTuple {}

impl SrgbaTuple {
    pub fn from_named(name: &str) -> Option<Self> {
        csscolorparser::parse(name).ok().map(|c| {
            SrgbaTuple(c.r as f64, c.g as f64, c.b as f64, c.a as f64)
        })
    }

    pub fn to_linear(&self) -> LinearRgba {
        fn linearize(v: f64) -> f64 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        LinearRgba(
            linearize(self.0),
            linearize(self.1),
            linearize(self.2),
            self.3,
        )
    }

    pub fn to_srgb_u8(&self) -> (u8, u8, u8, u8) {
        (
            (self.0 * 255.0).round() as u8,
            (self.1 * 255.0).round() as u8,
            (self.2 * 255.0).round() as u8,
            (self.3 * 255.0).round() as u8,
        )
    }

    pub fn to_x11_16bit_rgb_string(&self) -> String {
        let (r, g, b, _) = self.to_srgb_u8();
        format!(
            "#{:04x}{:04x}{:04x}",
            (r as u16) * 257,
            (g as u16) * 257,
            (b as u16) * 257,
        )
    }
}

impl From<(u8, u8, u8)> for SrgbaTuple {
    fn from(v: (u8, u8, u8)) -> Self {
        SrgbaTuple(
            v.0 as f64 / 255.0,
            v.1 as f64 / 255.0,
            v.2 as f64 / 255.0,
            1.0,
        )
    }
}

impl From<(u8, u8, u8, u8)> for SrgbaTuple {
    fn from(v: (u8, u8, u8, u8)) -> Self {
        SrgbaTuple(
            v.0 as f64 / 255.0,
            v.1 as f64 / 255.0,
            v.2 as f64 / 255.0,
            v.3 as f64 / 255.0,
        )
    }
}

impl core::hash::Hash for SrgbaTuple {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
        self.1.to_bits().hash(state);
        self.2.to_bits().hash(state);
        self.3.to_bits().hash(state);
    }
}

impl core::str::FromStr for SrgbaTuple {
    type Err = csscolorparser::ParseColorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        csscolorparser::parse(s).map(|c| SrgbaTuple(c.r as f64, c.g as f64, c.b as f64, c.a as f64))
    }
}

impl fmt::Display for SrgbaTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rgb({:.0},{:.0},{:.0})", self.0 * 255., self.1 * 255., self.2 * 255.)
    }
}

/// A linear RGBA tuple
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearRgba(pub f64, pub f64, pub f64, pub f64);
