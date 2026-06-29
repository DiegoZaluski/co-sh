use crate::core::border::{
    BorderStyle, BorderSidesConfig, BorderSide,
    is_valid_border_style, parse_border_style, border_chars,
    get_border_from_sides, get_border_sides,
    SINGLE, DOUBLE, ROUNDED, HEAVY, VALID_BORDER_STYLES,
};

#[test]
fn test_valid_border_styles_accepts_all() {
    assert!(is_valid_border_style("single"));
    assert!(is_valid_border_style("double"));
    assert!(is_valid_border_style("rounded"));
    assert!(is_valid_border_style("heavy"));
}

#[test]
fn test_valid_border_styles_rejects() {
    assert!(!is_valid_border_style("none"));
    assert!(!is_valid_border_style(""));
}

#[test]
fn test_valid_border_styles_constant() {
    assert_eq!(VALID_BORDER_STYLES.len(), 4);
}

#[test]
fn test_parse_border_style_valid() {
    assert_eq!(parse_border_style(Some("single"), BorderStyle::Single), BorderStyle::Single);
    assert_eq!(parse_border_style(Some("double"), BorderStyle::Rounded), BorderStyle::Double);
    assert_eq!(parse_border_style(Some("rounded"), BorderStyle::Single), BorderStyle::Rounded);
    assert_eq!(parse_border_style(Some("heavy"), BorderStyle::Single), BorderStyle::Heavy);
}

#[test]
fn test_parse_border_style_none_returns_fallback() {
    assert_eq!(parse_border_style(None, BorderStyle::Rounded), BorderStyle::Rounded);
}

#[test]
fn test_border_chars_mapping() {
    assert_eq!(*border_chars(BorderStyle::Single), SINGLE);
    assert_eq!(*border_chars(BorderStyle::Double), DOUBLE);
    assert_eq!(*border_chars(BorderStyle::Rounded), ROUNDED);
    assert_eq!(*border_chars(BorderStyle::Heavy), HEAVY);
}

#[test]
fn test_single_chars() {
    let c = SINGLE;
    assert_eq!(c.top_left, '┌');
    assert_eq!(c.top_right, '┐');
    assert_eq!(c.bottom_left, '└');
    assert_eq!(c.bottom_right, '┘');
    assert_eq!(c.horizontal, '─');
    assert_eq!(c.vertical, '│');
    assert_eq!(c.cross, '┼');
}

#[test]
fn test_double_chars() {
    assert_eq!(DOUBLE.top_left, '╔');
    assert_eq!(DOUBLE.top_right, '╗');
    assert_eq!(DOUBLE.horizontal, '═');
    assert_eq!(DOUBLE.vertical, '║');
}

#[test]
fn test_rounded_chars() {
    assert_eq!(ROUNDED.top_left, '╭');
    assert_eq!(ROUNDED.top_right, '╮');
}

#[test]
fn test_heavy_chars() {
    assert_eq!(HEAVY.top_left, '┏');
    assert_eq!(HEAVY.top_right, '┓');
    assert_eq!(HEAVY.horizontal, '━');
    assert_eq!(HEAVY.vertical, '┃');
}

#[test]
fn test_border_sides_all() {
    let all = BorderSidesConfig::ALL;
    assert!(all.top && all.right && all.bottom && all.left);
}

#[test]
fn test_border_sides_none() {
    let none = BorderSidesConfig::NONE;
    assert!(!none.top && !none.right && !none.bottom && !none.left);
}

#[test]
fn test_get_border_from_sides_all() {
    let sides = get_border_from_sides(BorderSidesConfig::ALL);
    assert_eq!(sides.len(), 4);
    assert!(sides.contains(&BorderSide::Top));
    assert!(sides.contains(&BorderSide::Bottom));
}

#[test]
fn test_get_border_from_sides_some() {
    let sides = get_border_from_sides(
        BorderSidesConfig { top: true, right: false, bottom: true, left: false },
    );
    assert_eq!(sides, vec![BorderSide::Top, BorderSide::Bottom]);
}

#[test]
fn test_get_border_from_sides_none() {
    let sides = get_border_from_sides(BorderSidesConfig::NONE);
    assert!(sides.is_empty());
}

#[test]
fn test_get_border_sides_identity() {
    let cfg = BorderSidesConfig { top: true, right: false, bottom: true, left: false };
    assert_eq!(get_border_sides(&cfg), cfg);
}
