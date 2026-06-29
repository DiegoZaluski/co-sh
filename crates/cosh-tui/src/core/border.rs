use crate::core::rgba::ColorInput;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderCharacters {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
    pub top_t: char,
    pub bottom_t: char,
    pub left_t: char,
    pub right_t: char,
    pub cross: char,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BorderStyle {
    Single,
    Double,
    Rounded,
    Heavy,
}

pub const VALID_BORDER_STYLES: [BorderStyle; 4] = [
    BorderStyle::Single,
    BorderStyle::Double,
    BorderStyle::Rounded,
    BorderStyle::Heavy,
];

pub fn is_valid_border_style(value: &str) -> bool {
    matches!(value, "single" | "double" | "rounded" | "heavy")
}

pub fn parse_border_style(value: Option<&str>, fallback: BorderStyle) -> BorderStyle {
    match value {
        Some("single") => BorderStyle::Single,
        Some("double") => BorderStyle::Double,
        Some("rounded") => BorderStyle::Rounded,
        Some("heavy") => BorderStyle::Heavy,
        Some(other) => {
            eprintln!("Invalid borderStyle \"{other}\", falling back to \"{fallback:?}\".");
            fallback
        }
        None => fallback,
    }
}

pub const SINGLE: BorderCharacters = BorderCharacters {
    top_left: '┌',
    top_right: '┐',
    bottom_left: '└',
    bottom_right: '┘',
    horizontal: '─',
    vertical: '│',
    top_t: '┬',
    bottom_t: '┴',
    left_t: '├',
    right_t: '┤',
    cross: '┼',
};

pub const DOUBLE: BorderCharacters = BorderCharacters {
    top_left: '╔',
    top_right: '╗',
    bottom_left: '╚',
    bottom_right: '╝',
    horizontal: '═',
    vertical: '║',
    top_t: '╦',
    bottom_t: '╩',
    left_t: '╠',
    right_t: '╣',
    cross: '╬',
};

pub const ROUNDED: BorderCharacters = BorderCharacters {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    horizontal: '─',
    vertical: '│',
    top_t: '┬',
    bottom_t: '┴',
    left_t: '├',
    right_t: '┤',
    cross: '┼',
};

pub const HEAVY: BorderCharacters = BorderCharacters {
    top_left: '┏',
    top_right: '┓',
    bottom_left: '┗',
    bottom_right: '┛',
    horizontal: '━',
    vertical: '┃',
    top_t: '┳',
    bottom_t: '┻',
    left_t: '┣',
    right_t: '┫',
    cross: '╋',
};

pub fn border_chars(style: BorderStyle) -> &'static BorderCharacters {
    match style {
        BorderStyle::Single => &SINGLE,
        BorderStyle::Double => &DOUBLE,
        BorderStyle::Rounded => &ROUNDED,
        BorderStyle::Heavy => &HEAVY,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BorderConfig {
    pub border_style: BorderStyle,
    pub border: BorderSidesConfig,
    pub border_color: Option<ColorInput>,
    pub custom_border_chars: Option<BorderCharacters>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoxDrawOptions {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub border_style: BorderStyle,
    pub border: BorderSidesConfig,
    pub border_color: ColorInput,
    pub custom_border_chars: Option<BorderCharacters>,
    pub background_color: ColorInput,
    pub should_fill: Option<bool>,
    pub title: Option<String>,
    pub title_alignment: Option<TitleAlignment>,
    pub bottom_title: Option<String>,
    pub bottom_title_alignment: Option<TitleAlignment>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TitleAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSidesConfig {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl BorderSidesConfig {
    pub const ALL: BorderSidesConfig = BorderSidesConfig {
        top: true,
        right: true,
        bottom: true,
        left: true,
    };

    pub const NONE: BorderSidesConfig = BorderSidesConfig {
        top: false,
        right: false,
        bottom: false,
        left: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BorderSide {
    Top,
    Right,
    Bottom,
    Left,
}

pub fn get_border_from_sides(sides: BorderSidesConfig) -> Vec<BorderSide> {
    let mut result = Vec::new();
    if sides.top { result.push(BorderSide::Top); }
    if sides.right { result.push(BorderSide::Right); }
    if sides.bottom { result.push(BorderSide::Bottom); }
    if sides.left { result.push(BorderSide::Left); }
    result
}

pub fn get_border_sides(border: &BorderSidesConfig) -> BorderSidesConfig {
    *border
}
