# `border` — border characters and styles

The building blocks for drawing boxes and tables: a set of 11 characters per
border style, four named styles, and the per-side toggles.

## `BorderCharacters`

A complete set of box-drawing characters:

```rust
pub struct BorderCharacters {
    pub top_left: char,     pub top_right: char,
    pub bottom_left: char,  pub bottom_right: char,
    pub horizontal: char,   pub vertical: char,
    pub top_t: char,        pub bottom_t: char,
    pub left_t: char,       pub right_t: char,
    pub cross: char,
}
```

The `*_t` characters are the "tee" junctions where a border meets a divider
(e.g. `┬`); `cross` is the four-way junction (`┼`).

## Named styles

```rust
pub enum BorderStyle { Single, Double, Rounded, Heavy }

pub const SINGLE: BorderCharacters;   // ┌─┐ │ └─┘ ┬ ┴ ├ ┤ ┼
pub const DOUBLE: BorderCharacters;   // ╔═╗ ║ ╚═╝ ╦ ╩ ╠ ╣ ╬
pub const ROUNDED: BorderCharacters;  // ╭─╮ │ ╰─╯ ┬ ┴ ├ ┤ ┼
pub const HEAVY: BorderCharacters;    // ┏━┓ ┃ ┗━┛ ┳ ┻ ┣ ┫ ╋

pub const fn border_chars(style: BorderStyle) -> &'static BorderCharacters
pub fn is_valid_border_style(value: &str) -> bool
pub fn parse_border_style(value: Option<&str>, fallback: BorderStyle) -> BorderStyle
```

- `border_chars` maps a style to its character set.
- `is_valid_border_style` accepts `"single" | "double" | "rounded" | "heavy"`.
- `parse_border_style` parses the same strings, printing a warning to stderr
  and returning the `fallback` for anything else (or `None`).

## Per-side configuration

```rust
pub struct BorderSidesConfig { pub top: bool, pub right: bool, pub bottom: bool, pub left: bool }
// BorderSidesConfig::ALL — all four on
// BorderSidesConfig::NONE — all four off

pub enum BorderSide { Top, Right, Bottom, Left }
pub fn get_border_from_sides(sides: BorderSidesConfig) -> Vec<BorderSide>
pub const fn get_border_sides(border: &BorderSidesConfig) -> BorderSidesConfig
```

## Combined configs

```rust
pub struct BorderConfig {
    pub border_style: BorderStyle,
    pub border: BorderSidesConfig,
    pub border_color: Option<ColorInput>,
    pub custom_border_chars: Option<BorderCharacters>,   // overrides the style's chars
}

pub struct BoxDrawOptions {
    pub x: i32, pub y: i32, pub width: i32, pub height: i32,
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

pub enum TitleAlignment { Left, Center, Right }
```

`BoxDrawOptions` is a complete description of a bordered box — the same
information `BoxRenderable` holds, but as a plain struct. `TitleAlignment`
controls where a box title sits along its border.

---

## Example — a custom border with only some sides

```rust,ignore
use cosh_tui::core::lib::border::{
    BorderSidesConfig, BorderStyle, BorderCharacters, border_chars, get_border_from_sides,
};

// Only a top and bottom line, no verticals.
let sides = BorderSidesConfig { top: true, right: false, bottom: true, left: false };
assert_eq!(get_border_from_sides(sides), vec![cosh_tui::core::border::BorderSide::Top, cosh_tui::core::border::BorderSide::Bottom]);

let chars = border_chars(BorderStyle::Rounded);
assert_eq!(chars.top_left, '╭');

// Fully custom characters override everything.
let custom = BorderCharacters { top_left: '+', top_right: '+', bottom_left: '+', bottom_right: '+', horizontal: '-', vertical: '|', top_t: '+', bottom_t: '+', left_t: '+', right_t: '+', cross: '+' };
```

`BoxRenderable` (in [`renderables`](../renderables/renderables.md)) uses these
types directly: `set_border_sides(BorderSidesConfig::ALL)`,
`set_border_style(BorderStyle::Rounded)`, `set_custom_border_chars(..)`.

Next: [styled_text — chunks and styled strings](styled_text.md).
