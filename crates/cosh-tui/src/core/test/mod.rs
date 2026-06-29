#![allow(
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    unused_must_use
)]

//! Unit tests for all implemented core modules.
//!
//! Tests use only **public APIs** — no access to private fields or constants.

// ─── types ────────────────────────────────────────────────────────────────

mod types_tests {
    use crate::core::types::{
        TextAttributes, get_base_attributes, ATTRIBUTE_BASE_BITS, ATTRIBUTE_BASE_MASK,
        DebugOverlayCorner, TargetChannel,
        WIDTH_METHOD_WCWIDTH, WIDTH_METHOD_UNICODE,
        TERMINAL_MULTIPLEXER_NONE, TERMINAL_MULTIPLEXER_TMUX,
        THEME_MODE_DARK, THEME_MODE_LIGHT,
        CURSOR_STYLE_BLOCK, CURSOR_STYLE_DEFAULT,
        MOUSE_POINTER_DEFAULT,
        TERMINAL_CAPABILITY_STATE_UNKNOWN, TERMINAL_CAPABILITY_STATE_SUPPORTED,
        TERMINAL_CAPABILITY_STATE_UNSUPPORTED,
    };

    #[test]
    fn test_text_attributes_bitflags() {
        assert_eq!(TextAttributes::NONE.bits(), 0);
        assert_eq!(TextAttributes::BOLD.bits(), 1);
        assert_eq!(TextAttributes::DIM.bits(), 2);
        assert_eq!(TextAttributes::ITALIC.bits(), 4);
        assert_eq!(TextAttributes::UNDERLINE.bits(), 8);
        assert_eq!(TextAttributes::BLINK.bits(), 16);
        assert_eq!(TextAttributes::INVERSE.bits(), 32);
        assert_eq!(TextAttributes::HIDDEN.bits(), 64);
        assert_eq!(TextAttributes::STRIKETHROUGH.bits(), 128);
    }

    #[test]
    fn test_text_attributes_combine() {
        let combined = TextAttributes::BOLD | TextAttributes::ITALIC;
        assert!(combined.contains(TextAttributes::BOLD));
        assert!(combined.contains(TextAttributes::ITALIC));
        assert_eq!(combined.bits(), 5);
    }

    #[test]
    fn test_get_base_attributes() {
        assert_eq!(get_base_attributes(0), 0);
        assert_eq!(get_base_attributes(255), 255);
        assert_eq!(get_base_attributes(256), 0);
    }

    #[test]
    fn test_constants_are_correct() {
        assert_eq!(ATTRIBUTE_BASE_BITS, 8);
        assert_eq!(ATTRIBUTE_BASE_MASK, 0xff);
        assert_eq!(THEME_MODE_DARK, "dark");
        assert_eq!(THEME_MODE_LIGHT, "light");
        assert_eq!(CURSOR_STYLE_BLOCK, "block");
        assert_eq!(CURSOR_STYLE_DEFAULT, "default");
        assert_eq!(MOUSE_POINTER_DEFAULT, "default");
        assert_eq!(WIDTH_METHOD_WCWIDTH, "wcwidth");
        assert_eq!(WIDTH_METHOD_UNICODE, "unicode");
        assert_eq!(TERMINAL_MULTIPLEXER_NONE, "none");
        assert_eq!(TERMINAL_MULTIPLEXER_TMUX, "tmux");
        assert_eq!(TERMINAL_CAPABILITY_STATE_UNKNOWN, "unknown");
        assert_eq!(TERMINAL_CAPABILITY_STATE_SUPPORTED, "supported");
        assert_eq!(TERMINAL_CAPABILITY_STATE_UNSUPPORTED, "unsupported");
    }

    #[test]
    fn test_debug_overlay_corner_discriminants() {
        assert_eq!(DebugOverlayCorner::TopLeft as i32, 0);
        assert_eq!(DebugOverlayCorner::TopRight as i32, 1);
        assert_eq!(DebugOverlayCorner::BottomLeft as i32, 2);
        assert_eq!(DebugOverlayCorner::BottomRight as i32, 3);
    }

    #[test]
    fn test_target_channel_discriminants() {
        assert_eq!(TargetChannel::Fg as i32, 1);
        assert_eq!(TargetChannel::Bg as i32, 2);
        assert_eq!(TargetChannel::Both as i32, 3);
    }
}

// ─── rgba ─────────────────────────────────────────────────────────────────

mod rgba_tests {
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
        // "xyz" has len 3, treated as shorthand: xxyyzz -> xx invalid hex -> 0
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
}

// ─── border ───────────────────────────────────────────────────────────────

mod border_tests {
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
}

// ─── utils ────────────────────────────────────────────────────────────────

mod utils_tests {
    use crate::core::utils::{
        create_text_attributes, TextAttributeOptions,
        attributes_with_link, get_link_id,
    };

    #[test]
    fn test_create_text_attributes_default_is_zero() {
        assert_eq!(create_text_attributes(TextAttributeOptions::default()), 0);
    }

    #[test]
    fn test_create_text_attributes_bold() {
        let opts = TextAttributeOptions { bold: true, ..Default::default() };
        assert_eq!(create_text_attributes(opts), 1);
    }

    #[test]
    fn test_create_text_attributes_all() {
        let opts = TextAttributeOptions {
            bold: true, italic: true, underline: true, dim: true,
            blink: true, inverse: true, hidden: true, strikethrough: true,
        };
        assert_eq!(create_text_attributes(opts), 0b1111_1111);
    }

    #[test]
    fn test_create_text_attributes_selection() {
        let opts = TextAttributeOptions {
            italic: true, inverse: true, ..Default::default()
        };
        assert_eq!(create_text_attributes(opts), 4 | 32);
    }

    #[test]
    fn test_attributes_with_link() {
        let linked = attributes_with_link(0, 42);
        assert_eq!(get_link_id(linked), 42);
        assert_eq!(linked & 0xff, 0);
    }

    #[test]
    fn test_attributes_with_link_preserves_base() {
        let base = create_text_attributes(TextAttributeOptions { bold: true, ..Default::default() });
        let linked = attributes_with_link(base, 255);
        assert_eq!(linked & 0xff, 1);
        assert_eq!(get_link_id(linked), 255);
    }

    #[test]
    fn test_get_link_id_zero() {
        assert_eq!(get_link_id(0), 0);
    }

    #[test]
    fn test_get_link_id_masked_to_24_bits() {
        let linked = attributes_with_link(0, 0x01_ff_ff_ff);
        assert_eq!(get_link_id(linked), 0x00_ff_ff_ff);
    }
}

// ─── renderable ───────────────────────────────────────────────────────────

mod renderable_tests {
    use crate::core::renderable::{Renderable, RootRenderable};

    #[test]
    fn test_root_renderable_new() {
        let root = RootRenderable::new();
        assert_eq!(root.id(), "__root__");
        assert!(root.num() > 0);
        assert!(root.is_visible());
        assert!(!root.is_focusable());
        assert!(!root.is_destroyed());
        assert!(root.parent_num().is_none());
    }

    #[test]
    fn test_root_renderable_default() {
        let root = RootRenderable::default();
        assert_eq!(root.id(), "__root__");
    }

    #[test]
    fn test_root_renderable_set_id() {
        let mut root = RootRenderable::new();
        root.set_id("custom-id".into());
        assert_eq!(root.id(), "custom-id");
    }

    #[test]
    fn test_root_renderable_set_visible() {
        let mut root = RootRenderable::new();
        root.set_visible(false);
        assert!(!root.is_visible());
    }

    #[test]
    fn test_root_renderable_set_focusable() {
        let mut root = RootRenderable::new();
        root.set_focusable(true);
        assert!(root.is_focusable());
    }

    #[test]
    fn test_root_renderable_add_child() {
        let mut root = RootRenderable::new();
        let idx = root.add_child(Box::new(RootRenderable::new()));
        assert_eq!(idx, 0);
        assert_eq!(root.children().len(), 1);
    }

    #[test]
    fn test_root_renderable_add_children() {
        let mut root = RootRenderable::new();
        root.add_child(Box::new(RootRenderable::new()));
        root.add_child(Box::new(RootRenderable::new()));
        assert_eq!(root.children().len(), 2);
    }

    #[test]
    fn test_root_renderable_remove_child() {
        let mut root = RootRenderable::new();
        root.add_child(Box::new(RootRenderable::new()));
        root.remove_child("__root__");
        assert_eq!(root.children().len(), 0);
    }

    #[test]
    fn test_root_renderable_remove_nonexistent() {
        let mut root = RootRenderable::new();
        root.remove_child("nonexistent");
    }

    #[test]
    fn test_root_renderable_insert_child_before() {
        let mut root = RootRenderable::new();
        root.add_child(Box::new(RootRenderable::new()));
        let result = root.insert_child_before(Box::new(RootRenderable::new()), "__root__");
        assert!(result.is_some());
        assert_eq!(root.children().len(), 2);
    }

    #[test]
    fn test_root_renderable_insert_before_nonexistent() {
        let mut root = RootRenderable::new();
        let result = root.insert_child_before(Box::new(RootRenderable::new()), "ghost");
        assert!(result.is_none());
    }

    #[test]
    fn test_root_renderable_children_count() {
        let mut root = RootRenderable::new();
        assert_eq!(root.children_count(), 0);
        root.add_child(Box::new(RootRenderable::new()));
        assert_eq!(root.children_count(), 1);
    }

    #[test]
    fn test_root_renderable_render_self_noop() {
        let root = RootRenderable::new();
        let area = ratatui::layout::Rect::new(0, 0, 10, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        root.render_self(&mut buf, area);
    }

    #[test]
    fn test_renderable_trait_object() {
        let root = RootRenderable::new();
        let trait_obj: &dyn Renderable = &root;
        assert_eq!(trait_obj.id(), "__root__");
        assert!(trait_obj.is_visible());
    }
}

// ─── syntax_style ─────────────────────────────────────────────────────────

#[allow(unused_must_use)]
mod syntax_style_tests {
    use crate::core::syntax_style::{
        SyntaxStyle, StyleDefinitionInput, ThemeTokenStyle, ThemeTokenStyleInner,
        convert_theme_to_styles,
    };
    use crate::core::rgba::ColorInput;

    const EMPTY_STYLE: StyleDefinitionInput = StyleDefinitionInput {
        fg: None, bg: None, bold: None, italic: None, underline: None, dim: None,
    };

    #[test]
    fn test_create_empty() {
        let ss = SyntaxStyle::create();
        assert_eq!(ss.get_style_count(), 0);
        assert_eq!(ss.get_cache_size(), 0);
    }

    #[test]
    fn test_register_style() {
        let mut ss = SyntaxStyle::create();
        let id = ss.register_style("keyword", &EMPTY_STYLE);
        assert_eq!(id, 1);
        assert_eq!(ss.get_style_count(), 1);
    }

    #[test]
    fn test_register_consecutive_ids() {
        let mut ss = SyntaxStyle::create();
        let id1 = ss.register_style("a", &EMPTY_STYLE);
        let id2 = ss.register_style("b", &EMPTY_STYLE);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn test_register_with_colors() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("keyword", &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bold: Some(true),
            bg: None, italic: None, underline: None, dim: None,
        });
        let style = ss.get_style("keyword");
        assert!(style.is_some());
        let s = style.unwrap();
        assert!(s.fg.is_some());
        assert_eq!(s.fg.unwrap().to_ints(), (255, 0, 0, 255));
        assert_eq!(s.bold, Some(true));
    }

    #[test]
    fn test_resolve_style_id() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("keyword", &EMPTY_STYLE);
        assert_eq!(ss.resolve_style_id("keyword"), Some(1));
        assert_eq!(ss.resolve_style_id("unknown"), None);
    }

    #[test]
    fn test_get_style_id_with_dot_fallback() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("keyword", &EMPTY_STYLE);
        assert_eq!(ss.get_style_id("keyword.rust"), Some(1));
        assert_eq!(ss.get_style_id("unknown.rust"), None);
    }

    #[test]
    fn test_get_style_unknown() {
        let ss = SyntaxStyle::create();
        assert!(ss.get_style("nonexistent").is_none());
    }

    #[test]
    fn test_get_style_with_dot_fallback() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("keyword", &EMPTY_STYLE);
        assert!(ss.get_style("keyword.rust").is_some());
    }

    #[test]
    fn test_merge_styles_single() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("a", &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bold: Some(true),
            bg: None, italic: None, underline: None, dim: None,
        });
        let merged = ss.merge_styles(&["a"]);
        assert!(merged.fg.is_some());
        assert!(merged.bg.is_none());
    }

    #[test]
    fn test_merge_styles_multiple() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("a", &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bg: None, bold: None, italic: None, underline: None, dim: None,
        });
        ss.register_style("b", &StyleDefinitionInput {
            bg: Some(ColorInput::String("blue".into())),
            italic: Some(true),
            fg: None, bold: None, underline: None, dim: None,
        });
        let merged = ss.merge_styles(&["a", "b"]);
        assert_eq!(merged.fg.unwrap().to_ints(), (255, 0, 0, 255));
        assert_eq!(merged.bg.unwrap().to_ints(), (0, 0, 255, 255));
    }

    #[test]
    fn test_merge_styles_caches() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("a", &EMPTY_STYLE);
        ss.merge_styles(&["a"]);
        assert_eq!(ss.get_cache_size(), 1);
        ss.merge_styles(&["a"]);
        assert_eq!(ss.get_cache_size(), 1);
    }

    #[test]
    fn test_clear_cache() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("a", &EMPTY_STYLE);
        ss.merge_styles(&["a"]);
        assert_eq!(ss.get_cache_size(), 1);
        ss.clear_cache();
        assert_eq!(ss.get_cache_size(), 0);
    }

    #[test]
    fn test_clear_name_cache() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("test", &EMPTY_STYLE);
        assert!(ss.resolve_style_id("test").is_some());
        ss.clear_name_cache();
        // Both resolve_style_id and get_style_id use name_cache
        assert!(ss.resolve_style_id("test").is_none());
        assert!(ss.get_style_id("test").is_none());
    }

    #[test]
    fn test_get_all_styles() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("a", &EMPTY_STYLE);
        ss.register_style("b", &EMPTY_STYLE);
        let all = ss.get_all_styles();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("a"));
    }

    #[test]
    fn test_get_registered_names() {
        let mut ss = SyntaxStyle::create();
        ss.register_style("x", &EMPTY_STYLE);
        ss.register_style("y", &EMPTY_STYLE);
        let names = ss.get_registered_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"x".to_string()));
    }

    #[test]
    fn test_destroy_clears_internals() {
        let mut ss = SyntaxStyle::create();
        let _ = ss.register_style("a", &EMPTY_STYLE);
        assert_eq!(ss.get_style_count(), 1);
        // After destroy, guard() panics on any method call
        ss.destroy();
    }

    #[test]
    fn test_destroy_twice_is_safe() {
        let mut ss = SyntaxStyle::create();
        ss.destroy();
        ss.destroy();
    }

    #[test]
    #[should_panic(expected = "SyntaxStyle is destroyed")]
    fn test_destroyed_panics() {
        let mut ss = SyntaxStyle::create();
        ss.destroy();
        ss.get_style_count();
    }

    #[test]
    fn test_from_styles() {
                let styles = {
            let mut m = std::collections::HashMap::new();
            m.insert("keyword".into(), StyleDefinitionInput {
                fg: Some(ColorInput::String("red".into())),
                bold: Some(true),
                bg: None, italic: None, underline: None, dim: None,
            });
            m
        };
        let ss = SyntaxStyle::from_styles(&styles);
        assert_eq!(ss.get_style_count(), 1);
    }

    #[test]
    fn test_convert_theme_to_styles_empty() {
        assert!(convert_theme_to_styles(&[]).is_empty());
    }

    #[test]
    fn test_convert_theme_to_styles_single() {
        let theme = vec![ThemeTokenStyle {
            scope: vec!["keyword".into()],
            style: ThemeTokenStyleInner {
                foreground: Some(ColorInput::String("red".into())),
                background: None,
                bold: None, italic: None, underline: None, dim: None,
            },
        }];
        let styles = convert_theme_to_styles(&theme);
        assert_eq!(styles.len(), 1);
        assert!(styles.contains_key("keyword"));
    }

    #[test]
    fn test_convert_theme_to_styles_multi_scope() {
        let theme = vec![ThemeTokenStyle {
            scope: vec!["keyword".into(), "keyword.rust".into()],
            style: ThemeTokenStyleInner {
                foreground: Some(ColorInput::String("blue".into())),
                background: None,
                bold: Some(true),
                italic: None, underline: None, dim: None,
            },
        }];
        let styles = convert_theme_to_styles(&theme);
        assert_eq!(styles.len(), 2);
    }

    #[test]
    fn test_from_theme() {
        let theme = vec![ThemeTokenStyle {
            scope: vec!["keyword".into()],
            style: ThemeTokenStyleInner {
                foreground: Some(ColorInput::String("green".into())),
                background: None, bold: None, italic: None, underline: None, dim: None,
            },
        }];
        let mut ss = SyntaxStyle::from_theme(&theme);
        assert_eq!(ss.get_style_count(), 1);
        let merged = ss.merge_styles(&["keyword"]);
        assert!(merged.fg.is_some());
    }
}

// ─── renderer ─────────────────────────────────────────────────────────────

mod renderer_tests {
    use std::time::Duration;

    use crate::core::renderer::{
        RendererConfig, RendererFrameEvent, RendererStats,
        ScreenMode, ExternalOutputMode, ConsoleMode, PixelResolution,
    };

    #[test]
    fn test_screen_mode_alternate() {
        assert!(matches!(ScreenMode::AlternateScreen, ScreenMode::AlternateScreen));
    }

    #[test]
    fn test_screen_mode_main() {
        assert!(matches!(ScreenMode::MainScreen, ScreenMode::MainScreen));
    }

    #[test]
    fn test_screen_mode_split_footer() {
        assert!(matches!(ScreenMode::SplitFooter { footer_height: 10 }, ScreenMode::SplitFooter { footer_height: 10 }));
    }

    #[test]
    fn test_external_output_mode() {
        assert!(matches!(ExternalOutputMode::CaptureStdout, ExternalOutputMode::CaptureStdout));
    }

    #[test]
    fn test_console_mode() {
        assert!(matches!(ConsoleMode::Disabled, ConsoleMode::Disabled));
    }

    #[test]
    fn test_renderer_config_default() {
        let cfg = RendererConfig::default();
        assert!(cfg.alternate_screen);
        assert_eq!(cfg.width, 80);
        assert_eq!(cfg.height, 24);
        assert_eq!(cfg.target_fps, 30);
        assert_eq!(cfg.max_fps, 60);
        assert!(cfg.exit_on_ctrl_c);
        assert!(cfg.clear_on_shutdown);
        assert!(cfg.enable_mouse_movement);
        assert!(cfg.use_mouse);
        assert!(cfg.auto_focus);
        assert!(cfg.background_color.is_none());
        assert_eq!(cfg.max_stat_samples, 300);
        assert_eq!(cfg.debounce_delay, Duration::from_millis(100));
        assert_eq!(cfg.memory_snapshot_interval, Duration::from_secs(0));
    }

    #[test]
    fn test_pixel_resolution() {
        let res = PixelResolution { width: 1920, height: 1080 };
        assert_eq!(res.width, 1920);
        assert_eq!(res.height, 1080);
    }

    #[test]
    fn test_renderer_frame_event() {
        let ev = RendererFrameEvent { frame_id: 42 };
        assert_eq!(ev.frame_id, 42);
    }

    #[test]
    fn test_renderer_stats() {
        let stats = RendererStats {
            fps: 30.0, frame_count: 100,
            frame_times: vec![16.0, 17.0],
            average_frame_time: 16.5, min_frame_time: 16.0, max_frame_time: 17.0,
        };
        assert!((stats.fps - 30.0).abs() < 0.001);
        assert_eq!(stats.frame_count, 100);
        assert_eq!(stats.frame_times.len(), 2);
    }
}
