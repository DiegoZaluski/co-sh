# `rgba` — colors

`RGBA` is the color type used throughout cosh-tui. It stores an 8-bit-per-
channel color plus an *intent* (how the color was specified) and an optional
palette slot, so colors can round-trip between RGB, indexed (ANSI-256), and
"terminal default" representations.

```rust
pub struct RGBA { /* r, g, b, a: u8, intent: ColorIntent, slot: u8 */ }

pub enum ColorIntent { Rgb, Indexed, Default }
```

## Constructing colors

| Constructor | Meaning |
|---|---|
| `from_ints(r, g, b, a)` | 8-bit channels, `0..=255` each. `const`. |
| `from_values(r, g, b, a)` | Float channels, each clamped to `0.0..=1.0`. |
| `from_hex("#rrggbb" \| "#rrggbbaa")` | Hex string; also accepts 3- and 4-digit shorthand (`#rgb`, `#rgba`). |
| `from_index(index, snapshot)` | ANSI-256 index; `snapshot` optionally overrides the color map. |
| `default_foreground(snapshot)` / `default_background(snapshot)` | The terminal's defaults. |

Hex parsing quirks worth knowing:

- A leading `#` is optional and stripped.
- `#abc` expands to `#aabbcc`; `#abcd` expands to `#aabbccdd`.
- Invalid input (bad length or non-hex digits) returns **magenta**
  `RGBA::from_values(1.0, 0.0, 1.0, 1.0)` — the classic "invalid color"
  sentinel — rather than failing.
- `rgb_to_hex(&RGBA) -> String` produces `#rrggbb` (or `#rrggbbaa` when
  alpha ≠ 255).

## Reading colors

- `to_ints() -> (u8, u8, u8, u8)` — raw channels.
- `r() / g() / b() / a() -> f32` — normalized `0.0..=1.0`.
- `map(f) -> (R, R, R, R)` — apply a function to all four channels.
- `equals(other: Option<&Self>) -> bool` — equality against an `Option`
  (handy for `Option<RGBA>` fields).
- `Display` — prints `rgba(0.50, 0.20, 0.10, 1.00)`.

Setters `set_r/set_g/set_b/set_a(f32)` mutate a channel, clamping to
`0.0..=1.0`.

## ANSI-256 conversion

```rust
pub const fn ansi256_index_to_rgb(index: u8) -> RgbTriplet  // (u8, u8, u8)
```

Maps an ANSI-256 index (0–255) to its RGB triple: 0–15 are the ANSI-16
palette, 16–231 are the 6×6×6 color cube, 232–255 are the grayscale ramp.
`normalize_indexed_color_index(index)` is the identity today but exists so the
index can be normalized centrally.

## `ColorInput` — where colors come from

Widgets accept colors as `Option<ColorInput>`:

```rust
pub enum ColorInput {
    String(String),   // "#ffcc00", "red", "transparent", …
    RGBA(RGBA),
}
```

- `From<&str>` and `From<RGBA>` convert into `ColorInput`.
- `parse_color(ColorInput) -> RGBA` resolves it: `"transparent"` → alpha 0,
  known CSS names → their hex, everything else → parsed as hex.
- `normalize_color_value(Option<ColorInput>) -> Option<NormalizedColorValue>`
  parses an `Option`, keeping it wrapped.

Recognized CSS names include the 16 HTML colors, the extended set
(`silver`, `maroon`, `olive`, `lime`, `aqua`, `teal`, `navy`, `fuchsia`,
`purple`, `orange`, …) and `bright*` variants (`brightred`, `brightgreen`,
…). Lookup is case-insensitive.

## HSV

```rust
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> RGBA
```

Standard HSV→RGB conversion (`h` in degrees) returning an opaque `RGBA`.

---

## Example

```rust,ignore
use cosh_tui::core::lib::rgba::{RGBA, ColorInput, parse_color};

let a = RGBA::from_hex("#ff8800");            // opaque orange
let b = RGBA::from_ints(255, 136, 0, 255);
assert_eq!(a, b);

let named = parse_color(ColorInput::String("orange".into()));
assert_eq!(named, RGBA::from_hex("#ffa500"));

let transparent = parse_color(ColorInput::String("transparent".into()));
assert_eq!(transparent.to_ints(), (0, 0, 0, 0));

assert_eq!(RGBA::from_hex("#nope").to_ints(), (255, 0, 255, 255)); // magenta sentinel
```

Next: [border — border characters and styles](border.md).
