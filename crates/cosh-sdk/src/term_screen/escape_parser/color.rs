#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::items_after_statements
)]
pub use crate::term_screen::color_types::{LinearRgba, SrgbaTuple};
use num_derive::FromPrimitive;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, FromPrimitive, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
/// These correspond to the classic ANSI color indices and are
/// used for convenience/readability in code
pub enum AnsiColor {
    /// "Dark" black
    Black = 0,
    /// Dark red
    Maroon,
    /// Dark green
    Green,
    /// "Dark" yellow
    Olive,
    /// Dark blue
    Navy,
    /// Dark purple
    Purple,
    /// "Dark" cyan
    Teal,
    /// "Dark" white
    Silver,
    /// "Bright" black
    Grey,
    /// Bright red
    Red,
    /// Bright green
    Lime,
    /// Bright yellow
    Yellow,
    /// Bright blue
    Blue,
    /// Bright purple
    Fuchsia,
    /// Bright Cyan/Aqua
    Aqua,
    /// Bright white
    White,
}

impl From<AnsiColor> for u8 {
    fn from(col: AnsiColor) -> Self {
        col as Self
    }
}

/// Describes a color in the SRGB colorspace using red, green and blue
/// components in the range 0-255.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub struct RgbColor {
    bits: u32,
}

impl From<RgbColor> for SrgbaTuple {
    fn from(val: RgbColor) -> Self {
        val.to_tuple_rgba()
    }
}

impl RgbColor {
    /// Construct a color from discrete red, green, blue values
    /// in the range 0-255.
    #[must_use]
    pub const fn new_8bpc(red: u8, green: u8, blue: u8) -> Self {
        Self {
            bits: ((red as u32) << 16) | ((green as u32) << 8) | blue as u32,
        }
    }

    /// Construct a color from discrete red, green, blue values
    /// in the range 0.0-1.0 in the sRGB colorspace.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn new_f32(red: f32, green: f32, blue: f32) -> Self {
        let red = (red * 255.) as u8;
        let green = (green * 255.) as u8;
        let blue = (blue * 255.) as u8;
        Self::new_8bpc(red, green, blue)
    }

    /// Returns red, green, blue as 8bpc values.
    /// Will convert from 10bpc if that is the internal storage.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn to_tuple_rgb8(self) -> (u8, u8, u8) {
        (
            (self.bits >> 16) as u8,
            (self.bits >> 8) as u8,
            self.bits as u8,
        )
    }

    /// Returns red, green, blue as floating point values in the range 0.0-1.0.
    /// An alpha channel with the value of 1.0 is included.
    /// The values are in the sRGB colorspace.
    #[must_use]
    pub fn to_tuple_rgba(self) -> SrgbaTuple {
        SrgbaTuple(
            f64::from((self.bits >> 16) as u8) / 255.0,
            f64::from((self.bits >> 8) as u8) / 255.0,
            f64::from(self.bits as u8) / 255.0,
            1.0,
        )
    }

    /// Returns red, green, blue as floating point values in the range 0.0-1.0.
    /// An alpha channel with the value of 1.0 is included.
    /// The values are converted from sRGB to linear colorspace.
    #[must_use]
    pub fn to_linear_tuple_rgba(self) -> LinearRgba {
        self.to_tuple_rgba().to_linear()
    }

    /// Construct a color from an X11/SVG/CSS3 color name.
    /// Returns None if the supplied name is not recognized.
    /// The list of names can be found here:
    /// <https://en.wikipedia.org/wiki/X11_color_names>
    #[must_use]
    pub fn from_named(name: &str) -> Option<Self> {
        Some(SrgbaTuple::from_named(name)?.into())
    }

    /// Returns a string of the form `#RRGGBB`
    #[must_use]
    pub fn to_rgb_string(self) -> String {
        let (red, green, blue) = self.to_tuple_rgb8();
        format!("#{red:02x}{green:02x}{blue:02x}")
    }

    /// Returns a string of the form `rgb:RRRR/GGGG/BBBB`
    #[must_use]
    pub fn to_x11_16bit_rgb_string(self) -> String {
        let (red, green, blue) = self.to_tuple_rgb8();
        format!("rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}")
    }

    /// Construct a color from a string of the form `#RRGGBB` where
    /// R, G and B are all hex digits.
    /// `hsl:hue sat light` is also accepted, and allows specifying a color
    /// in the HSL color space, where `hue` is measure in degrees and has
    /// a range of 0-360, and both `sat` and `light` are specified in percentage
    /// in the range 0-100.
    #[must_use]
    #[allow(clippy::many_single_char_names, clippy::cast_possible_truncation)]
    pub fn from_rgb_str(s: &str) -> Option<Self> {
        // Handle hsl: prefix with space separators (original wezterm format)
        if let Some(hsl) = s.strip_prefix("hsl:") {
            let parts: Vec<f64> = hsl
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if parts.len() >= 3 {
                let c = csscolorparser::Color::from_hsla(
                    parts[0] as f32,
                    parts[1] as f32 / 100.,
                    parts[2] as f32 / 100.,
                    1.0,
                );
                let [r, g, b, _] = c.to_rgba8();
                return Some(Self::new_8bpc(r, g, b));
            }
            return None;
        }
        // Handle X11 rgb:RRRR/GGGG/BBBB format
        if let Some(rgb) = s.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb.split('/').collect();
            if parts.len() == 3 {
                let to_u8 = |hex: &str| -> Option<u8> {
                    let v = u32::from_str_radix(hex, 16).ok()?;
                    let max_val = (1u32 << (4 * hex.len())) - 1;
                    Some((v * 255 / max_val) as u8)
                };
                let r = to_u8(parts[0])?;
                let g = to_u8(parts[1])?;
                let b = to_u8(parts[2])?;
                return Some(Self::new_8bpc(r, g, b));
            }
            return None;
        }
        // Handle #RGB / #RRGGBB / #RRRGGGBBB / #RRRRGGGGBBBB X11 convention
        if let Some(hex) = s.strip_prefix('#') {
            let digits = hex.len();
            if digits % 3 == 0 {
                let per = digits / 3;
                if (1..=4).contains(&per) {
                    let to_u8 = |i: usize| -> Option<u8> {
                        let hex_part = &hex[i * per..(i + 1) * per];
                        let v = u32::from_str_radix(hex_part, 16).ok()?;
                        match per {
                            1 => Some((v as u8) << 4),
                            2 => Some(v as u8),
                            _ => {
                                let max_val = (1u32 << (4 * per)) - 1;
                                Some((v * 255 / max_val) as u8)
                            }
                        }
                    };
                    let r = to_u8(0)?;
                    let g = to_u8(1)?;
                    let b = to_u8(2)?;
                    return Some(Self::new_8bpc(r, g, b));
                }
            }
        }
        // Fallback to csscolorparser for standard CSS formats and named colors
        let srgb: SrgbaTuple = s.parse().ok()?;
        Some(srgb.into())
    }

    /// Construct a color from an SVG/CSS3 color name.
    /// or from a string of the form `#RRGGBB` where
    /// R, G and B are all hex digits.
    /// `hsl:hue sat light` is also accepted, and allows specifying a color
    /// in the HSL color space, where `hue` is measure in degrees and has
    /// a range of 0-360, and both `sat` and `light` are specified in percentage
    /// in the range 0-100.
    /// Returns None if the supplied name is not recognized.
    /// The list of names can be found here:
    /// <https://ogeon.github.io/docs/palette/master/palette/named/index.html>
    #[must_use]
    pub fn from_named_or_rgb_string(s: &str) -> Option<Self> {
        Self::from_rgb_str(s).or_else(|| Self::from_named(s))
    }
}

impl From<SrgbaTuple> for RgbColor {
    #[allow(clippy::cast_possible_truncation)]
    fn from(srgb: SrgbaTuple) -> Self {
        let SrgbaTuple(r, g, b, _) = srgb;
        Self::new_f32(r as f32, g as f32, b as f32)
    }
}

/// This is mildly unfortunate: in order to round trip `RgbColor` with serde
/// we need to provide a Serialize impl equivalent to the Deserialize impl
/// below.  We use the impl below to allow more flexible specification of
/// color strings in the config file.  A side effect of doing it this way
/// is that we have to serialize `RgbColor` as a 7-byte string when we could
/// otherwise serialize it as a 3-byte array.  There's probably a way
/// to make this work more efficiently, but for now this will do.
impl Serialize for RgbColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = self.to_rgb_string();
        s.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RgbColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_named_or_rgb_string(&s)
            .ok_or_else(|| format!("unknown color name: {s}"))
            .map_err(serde::de::Error::custom)
    }
}

/// An index into the fixed color palette.
pub type PaletteIndex = u8;

/// Specifies the color to be used when rendering a cell.
///
/// This differs from `ColorAttribute` in that this type can only
/// specify one of the possible color types at once, whereas the
/// `ColorAttribute` type can specify a `TrueColor` value and a fallback.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum ColorSpec {
    #[default]
    Default,
    /// Use either a raw number, or use values from the `AnsiColor` enum
    PaletteIndex(PaletteIndex),
    TrueColor(SrgbaTuple),
}

impl From<AnsiColor> for ColorSpec {
    fn from(col: AnsiColor) -> Self {
        Self::PaletteIndex(col as u8)
    }
}

impl From<RgbColor> for ColorSpec {
    fn from(col: RgbColor) -> Self {
        Self::TrueColor(col.into())
    }
}

impl From<SrgbaTuple> for ColorSpec {
    fn from(col: SrgbaTuple) -> Self {
        Self::TrueColor(col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn from_hsl() {
        let foo = RgbColor::from_rgb_str("hsl:235 100  50").unwrap();
        assert_eq!(foo.to_rgb_string(), "#0015ff");
    }

    #[test]
    fn from_rgb() {
        assert!(RgbColor::from_rgb_str("").is_none());
        assert!(RgbColor::from_rgb_str("#xyxyxy").is_none());

        let black = RgbColor::from_rgb_str("#FFF").unwrap();
        assert_eq!(black.to_tuple_rgb8(), (0xf0, 0xf0, 0xf0));

        let black = RgbColor::from_rgb_str("#000000").unwrap();
        assert_eq!(black.to_tuple_rgb8(), (0, 0, 0));

        let grey = RgbColor::from_rgb_str("rgb:D6/D6/D6").unwrap();
        assert_eq!(grey.to_tuple_rgb8(), (0xd6, 0xd6, 0xd6));

        let grey = RgbColor::from_rgb_str("rgb:f0f0/f0f0/f0f0").unwrap();
        assert_eq!(grey.to_tuple_rgb8(), (0xf0, 0xf0, 0xf0));
    }
}
