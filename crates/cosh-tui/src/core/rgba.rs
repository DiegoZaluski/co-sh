use std::fmt;
use std::sync::LazyLock;

pub type RgbTriplet = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorIntent {
    Rgb,
    Indexed,
    Default,
}

pub const DEFAULT_FOREGROUND_RGB: RgbTriplet = (255, 255, 255);
pub const DEFAULT_BACKGROUND_RGB: RgbTriplet = (0, 0, 0);

const ANSI16_RGB: [RgbTriplet; 16] = [
    (0x00, 0x00, 0x00),
    (0x80, 0x00, 0x00),
    (0x00, 0x80, 0x00),
    (0x80, 0x80, 0x00),
    (0x00, 0x00, 0x80),
    (0x80, 0x00, 0x80),
    (0x00, 0x80, 0x80),
    (0xc0, 0xc0, 0xc0),
    (0x80, 0x80, 0x80),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x00, 0x00, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

const ANSI_256_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedColorValue {
    pub rgba: RGBA,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RGBA {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    intent: ColorIntent,
    slot: u8,
}

fn to_u8(value: f32) -> u8 {
    (value.max(0.0).min(1.0) * 255.0).round() as u8
}

fn ansi256_index_to_rgb(index: u8) -> RgbTriplet {
    let normalized = normalize_indexed_color_index(index);

    if (normalized as usize) < ANSI16_RGB.len() {
        return ANSI16_RGB[normalized as usize];
    }

    if normalized < 232 {
        let cube_index = normalized - 16;
        let r = cube_index / 36;
        let g = (cube_index / 6) % 6;
        let b = cube_index % 6;
        (
            ANSI_256_CUBE_LEVELS[r as usize],
            ANSI_256_CUBE_LEVELS[g as usize],
            ANSI_256_CUBE_LEVELS[b as usize],
        )
    } else {
        let value = 8 + (normalized - 232) * 10;
        (value, value, value)
    }
}

fn rgba_for_ansi256_index(index: u8) -> RGBA {
    let (r, g, b) = ansi256_index_to_rgb(index);
    RGBA::from_ints(r, g, b, 255)
}

pub fn normalize_indexed_color_index(index: u8) -> u8 {
    index
}

#[allow(dead_code)]
fn get_fallback_ansi256_palette() -> &'static [RGBA; 256] {
    static PALETTE: LazyLock<[RGBA; 256]> = LazyLock::new(|| {
        let mut palette = [RGBA::from_ints(0, 0, 0, 255); 256];
        for i in 0..256u16 {
            let (r, g, b) = ansi256_index_to_rgb(i as u8);
            palette[i as usize] = RGBA::from_ints(r, g, b, 255);
        }
        palette
    });
    &PALETTE
}

impl RGBA {
    pub fn from_values(r: f32, g: f32, b: f32, a: f32) -> Self {
        RGBA {
            r: to_u8(r),
            g: to_u8(g),
            b: to_u8(b),
            a: to_u8(a),
            intent: ColorIntent::Rgb,
            slot: 0,
        }
    }

    pub fn clone(rgba: &RGBA) -> Self {
        *rgba
    }

    pub fn from_ints(r: u8, g: u8, b: u8, a: u8) -> Self {
        RGBA {
            r,
            g,
            b,
            a,
            intent: ColorIntent::Rgb,
            slot: 0,
        }
    }

    pub fn from_hex(hex: &str) -> Self {
        hex_to_rgb(hex)
    }

    pub fn from_index(index: u8, snapshot: Option<ColorInput>) -> Self {
        let normalized = normalize_indexed_color_index(index);
        let rgba = match snapshot {
            Some(input) => parse_color(input),
            None => rgba_for_ansi256_index(normalized),
        };
        RGBA {
            r: rgba.r,
            g: rgba.g,
            b: rgba.b,
            a: rgba.a,
            intent: ColorIntent::Indexed,
            slot: normalized,
        }
    }

    pub fn default_foreground(snapshot: Option<ColorInput>) -> Self {
        let rgba = match snapshot {
            Some(input) => parse_color(input),
            None => RGBA::from_ints(DEFAULT_FOREGROUND_RGB.0, DEFAULT_FOREGROUND_RGB.1, DEFAULT_FOREGROUND_RGB.2, 255),
        };
        RGBA {
            r: rgba.r,
            g: rgba.g,
            b: rgba.b,
            a: rgba.a,
            intent: ColorIntent::Default,
            slot: 0,
        }
    }

    pub fn default_background(snapshot: Option<ColorInput>) -> Self {
        let rgba = match snapshot {
            Some(input) => parse_color(input),
            None => RGBA::from_ints(DEFAULT_BACKGROUND_RGB.0, DEFAULT_BACKGROUND_RGB.1, DEFAULT_BACKGROUND_RGB.2, 255),
        };
        RGBA {
            r: rgba.r,
            g: rgba.g,
            b: rgba.b,
            a: rgba.a,
            intent: ColorIntent::Default,
            slot: 0,
        }
    }

    pub fn to_ints(&self) -> (u8, u8, u8, u8) {
        (self.r, self.g, self.b, self.a)
    }

    pub fn r(&self) -> f32 {
        self.r as f32 / 255.0
    }

    pub fn set_r(&mut self, value: f32) {
        self.r = to_u8(value);
    }

    pub fn g(&self) -> f32 {
        self.g as f32 / 255.0
    }

    pub fn set_g(&mut self, value: f32) {
        self.g = to_u8(value);
    }

    pub fn b(&self) -> f32 {
        self.b as f32 / 255.0
    }

    pub fn set_b(&mut self, value: f32) {
        self.b = to_u8(value);
    }

    pub fn a(&self) -> f32 {
        self.a as f32 / 255.0
    }

    pub fn set_a(&mut self, value: f32) {
        self.a = to_u8(value);
    }

    pub fn map<R>(&self, f: impl Fn(f32) -> R) -> (R, R, R, R) {
        (f(self.r()), f(self.g()), f(self.b()), f(self.a()))
    }

    pub fn equals(&self, other: Option<&RGBA>) -> bool {
        match other {
            Some(other) => self == other,
            None => false,
        }
    }
}

impl fmt::Display for RGBA {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rgba({:.2}, {:.2}, {:.2}, {:.2})", self.r(), self.g(), self.b(), self.a())
    }
}

pub fn normalize_color_value(value: Option<ColorInput>) -> Option<NormalizedColorValue> {
    value.map(|v| NormalizedColorValue { rgba: parse_color(v) })
}

pub fn hex_to_rgb(hex: &str) -> RGBA {
    let hex = hex.trim_start_matches('#');

    let hex = match hex.len() {
        3 => format!("{}{}{}{}{}{}", &hex[0..1], &hex[0..1], &hex[1..2], &hex[1..2], &hex[2..3], &hex[2..3]),
        4 => {
            let h = hex.as_bytes();
            format!("{}{}{}{}{}{}{}{}", h[0] as char, h[0] as char, h[1] as char, h[1] as char, h[2] as char, h[2] as char, h[3] as char, h[3] as char)
        }
        _ => hex.to_string(),
    };

    if hex.len() != 6 && hex.len() != 8 {
        return RGBA::from_values(1.0, 0.0, 1.0, 1.0);
    }

    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    let a = if hex.len() == 8 {
        u8::from_str_radix(&hex[6..8], 16).unwrap_or(255)
    } else {
        255
    };

    RGBA::from_ints(r, g, b, a)
}

pub fn rgb_to_hex(rgb: &RGBA) -> String {
    let (r, g, b, a) = rgb.to_ints();
    if a == 255 {
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a)
    }
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> RGBA {
    let i = (h / 60.0).floor() as i32 % 6;
    let f = h / 60.0 - (h / 60.0).floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);

    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };

    RGBA::from_values(r, g, b, 1.0)
}

const CSS_COLOR_NAMES: &[(&str, &str)] = &[
    ("black", "#000000"),
    ("white", "#FFFFFF"),
    ("red", "#FF0000"),
    ("green", "#008000"),
    ("blue", "#0000FF"),
    ("yellow", "#FFFF00"),
    ("cyan", "#00FFFF"),
    ("magenta", "#FF00FF"),
    ("silver", "#C0C0C0"),
    ("gray", "#808080"),
    ("grey", "#808080"),
    ("maroon", "#800000"),
    ("olive", "#808000"),
    ("lime", "#00FF00"),
    ("aqua", "#00FFFF"),
    ("teal", "#008080"),
    ("navy", "#000080"),
    ("fuchsia", "#FF00FF"),
    ("purple", "#800080"),
    ("orange", "#FFA500"),
    ("brightblack", "#666666"),
    ("brightred", "#FF6666"),
    ("brightgreen", "#66FF66"),
    ("brightblue", "#6666FF"),
    ("brightyellow", "#FFFF66"),
    ("brightcyan", "#66FFFF"),
    ("brightmagenta", "#FF66FF"),
    ("brightwhite", "#FFFFFF"),
];

pub fn parse_color(color: ColorInput) -> RGBA {
    match color {
        ColorInput::String(s) => {
            let lower = s.to_lowercase();
            if lower == "transparent" {
                return RGBA::from_values(0.0, 0.0, 0.0, 0.0);
            }
            for (name, hex) in CSS_COLOR_NAMES {
                if *name == lower {
                    return hex_to_rgb(hex);
                }
            }
            hex_to_rgb(&s)
        }
        ColorInput::RGBA(rgba) => rgba,
    }
}

#[derive(Debug, Clone)]
pub enum ColorInput {
    String(String),
    RGBA(RGBA),
}

impl From<&str> for ColorInput {
    fn from(s: &str) -> Self {
        ColorInput::String(s.to_string())
    }
}

impl From<RGBA> for ColorInput {
    fn from(rgba: RGBA) -> Self {
        ColorInput::RGBA(rgba)
    }
}
