use crate::core::rgba::{
    RGBA, ColorInput,
    ansi256_index_to_rgb,
    rgb_to_hex, hsv_to_rgb, parse_color,
    DEFAULT_FOREGROUND_RGB, DEFAULT_BACKGROUND_RGB,
};

#[test]
fn test_from_values_clamps() {
    let c = RGBA::from_values(2.0, -0.5, 0.5, 0.0);
    let (r, g, b, a) = c.to_ints();
    assert_eq!(r, 255);
    assert_eq!(g, 0);
    assert_eq!(b, 128);
    assert_eq!(a, 0);
}

#[test]
fn test_from_ints_round_trip() {
    let c = RGBA::from_ints(10, 20, 30, 255);
    assert_eq!(c.to_ints(), (10, 20, 30, 255));
}

#[test]
fn test_from_hex_6_digit() {
    let c = RGBA::from_hex("#ff0000");
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_from_hex_8_digit_with_alpha() {
    let c = RGBA::from_hex("#ff000080");
    assert_eq!(c.to_ints(), (255, 0, 0, 128));
}

#[test]
fn test_from_hex_3_digit_shorthand() {
    let c = RGBA::from_hex("#f00");
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_from_hex_no_hash() {
    let c = RGBA::from_hex("00ff00");
    assert_eq!(c.to_ints(), (0, 255, 0, 255));
}

#[test]
fn test_from_hex_invalid_3char_parses_as_shorthand() {
    let c = RGBA::from_hex("xyz");
    assert_eq!(c.to_ints(), (0, 0, 0, 255));
}

#[test]
fn test_rgb_to_hex_opaque() {
    let c = RGBA::from_ints(255, 0, 0, 255);
    assert_eq!(rgb_to_hex(&c), "#ff0000");
}

#[test]
fn test_rgb_to_hex_with_alpha() {
    let c = RGBA::from_ints(255, 0, 0, 128);
    assert_eq!(rgb_to_hex(&c), "#ff000080");
}

#[test]
fn test_rgb_to_hex_round_trip() {
    let c = RGBA::from_ints(10, 20, 30, 255);
    let hex = rgb_to_hex(&c);
    let c2 = RGBA::from_hex(&hex);
    assert_eq!(c, c2);
}

#[test]
fn test_hsv_red() {
    let c = hsv_to_rgb(0.0, 1.0, 1.0);
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_hsv_green() {
    let c = hsv_to_rgb(120.0, 1.0, 1.0);
    assert_eq!(c.to_ints(), (0, 255, 0, 255));
}

#[test]
fn test_hsv_blue() {
    let c = hsv_to_rgb(240.0, 1.0, 1.0);
    assert_eq!(c.to_ints(), (0, 0, 255, 255));
}

#[test]
fn test_hsv_white() {
    let c = hsv_to_rgb(0.0, 0.0, 1.0);
    assert_eq!(c.to_ints(), (255, 255, 255, 255));
}

#[test]
fn test_hsv_black() {
    let c = hsv_to_rgb(0.0, 0.0, 0.0);
    assert_eq!(c.to_ints(), (0, 0, 0, 255));
}

#[test]
fn test_from_index_no_snapshot() {
    let c = RGBA::from_index(0, None);
    assert_eq!(c.to_ints(), (0, 0, 0, 255));
}

#[test]
fn test_from_index_15_no_snapshot() {
    let c = RGBA::from_index(15, None);
    assert_eq!(c.to_ints(), (255, 255, 255, 255));
}

#[test]
fn test_from_index_with_snapshot() {
    let c = RGBA::from_index(0, Some(ColorInput::String("#ff0000".into())));
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_ansi256_index_0_black() {
    assert_eq!(ansi256_index_to_rgb(0), (0, 0, 0));
}

#[test]
fn test_ansi256_index_15_white() {
    assert_eq!(ansi256_index_to_rgb(15), (255, 255, 255));
}

#[test]
fn test_ansi256_cube_start() {
    assert_eq!(ansi256_index_to_rgb(16), (0, 0, 0));
}

#[test]
fn test_ansi256_greyscale_232() {
    let (r, g, b) = ansi256_index_to_rgb(232);
    assert_eq!((r, g, b), (8, 8, 8));
}

#[test]
fn test_ansi256_greyscale_255() {
    let (r, g, b) = ansi256_index_to_rgb(255);
    assert_eq!((r, g, b), (238, 238, 238));
}

#[test]
fn test_default_foreground_no_snapshot() {
    let c = RGBA::default_foreground(None);
    assert_eq!(c.to_ints(), (255, 255, 255, 255));
}

#[test]
fn test_default_background_no_snapshot() {
    let c = RGBA::default_background(None);
    assert_eq!(c.to_ints(), (0, 0, 0, 255));
}

#[test]
fn test_default_foreground_with_snapshot() {
    let snapshot = Some(ColorInput::RGBA(RGBA::from_ints(1, 2, 3, 255)));
    let c = RGBA::default_foreground(snapshot);
    assert_eq!(c.to_ints(), (1, 2, 3, 255));
}

#[test]
fn test_parse_color_named_red() {
    let c = parse_color(ColorInput::String("red".into()));
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_parse_color_named_case_insensitive() {
    let c = parse_color(ColorInput::String("RED".into()));
    assert_eq!(c.to_ints(), (255, 0, 0, 255));
}

#[test]
fn test_parse_color_transparent() {
    let c = parse_color(ColorInput::String("transparent".into()));
    assert_eq!(c.to_ints(), (0, 0, 0, 0));
}

#[test]
fn test_parse_color_hex() {
    let c = parse_color(ColorInput::String("#00ff00".into()));
    assert_eq!(c.to_ints(), (0, 255, 0, 255));
}

#[test]
fn test_parse_color_rgba_direct() {
    let rgba = RGBA::from_ints(10, 20, 30, 255);
    let c = parse_color(ColorInput::RGBA(rgba));
    assert_eq!(c, rgba);
}

#[test]
fn test_float_accessors() {
    let c = RGBA::from_ints(128, 64, 32, 255);
    assert!((c.r() - 0.502).abs() < 0.002);
    assert!((c.g() - 0.251).abs() < 0.002);
    assert!((c.b() - 0.125).abs() < 0.002);
    assert!((c.a() - 1.0).abs() < 0.001);
}

#[test]
fn test_setters() {
    let mut c = RGBA::from_ints(0, 0, 0, 0);
    c.set_r(1.0);
    c.set_g(0.5);
    c.set_b(0.0);
    c.set_a(0.5);
    assert_eq!(c.to_ints(), (255, 128, 0, 128));
}

#[test]
fn test_equals() {
    let a = RGBA::from_ints(1, 2, 3, 4);
    let b = RGBA::from_ints(1, 2, 3, 4);
    let c = RGBA::from_ints(5, 6, 7, 8);
    assert!(a.equals(Some(&b)));
    assert!(!a.equals(Some(&c)));
    assert!(!a.equals(None::<&RGBA>));
}

#[test]
fn test_display() {
    let c = RGBA::from_ints(255, 0, 0, 255);
    assert_eq!(format!("{c}"), "rgba(1.00, 0.00, 0.00, 1.00)");
}

#[test]
fn test_default_constants() {
    assert_eq!(DEFAULT_FOREGROUND_RGB, (255, 255, 255));
    assert_eq!(DEFAULT_BACKGROUND_RGB, (0, 0, 0));
}

#[test]
fn test_color_input_from_str() {
    let ci: ColorInput = "blue".into();
    match ci {
        ColorInput::String(s) => assert_eq!(s, "blue"),
        _ => panic!("expected String variant"),
    }
}

#[test]
fn test_color_input_from_rgba() {
    let rgba = RGBA::from_ints(1, 2, 3, 4);
    let ci: ColorInput = rgba.into();
    match ci {
        ColorInput::RGBA(c) => assert_eq!(c.to_ints(), (1, 2, 3, 4)),
        _ => panic!("expected RGBA variant"),
    }
}

#[test]
fn test_not_a_color_name() {
    let c = parse_color(ColorInput::String("notacolor".into()));
    assert_eq!(c.to_ints(), (255, 0, 255, 255));
}
