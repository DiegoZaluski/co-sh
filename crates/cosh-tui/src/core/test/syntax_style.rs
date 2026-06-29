use crate::core::syntax_style::{
    StyleDefinitionInput, SyntaxStyle, ThemeTokenStyle, ThemeTokenStyleInner,
    convert_theme_to_styles,
};

use crate::core::rgba::ColorInput;

const EMPTY_STYLE: StyleDefinitionInput = StyleDefinitionInput {
    fg: None,
    bg: None,
    bold: None,
    italic: None,
    underline: None,
    dim: None,
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
    let _ = ss.register_style(
        "keyword",
        &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bold: Some(true),
            bg: None,
            italic: None,
            underline: None,
            dim: None,
        },
    );
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
    let _ = ss.register_style("keyword", &EMPTY_STYLE);
    assert_eq!(ss.resolve_style_id("keyword"), Some(1));
    assert_eq!(ss.resolve_style_id("unknown"), None);
}

#[test]
fn test_get_style_id_with_dot_fallback() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("keyword", &EMPTY_STYLE);
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
    let _ = ss.register_style("keyword", &EMPTY_STYLE);
    assert!(ss.get_style("keyword.rust").is_some());
}

#[test]
fn test_merge_styles_single() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style(
        "a",
        &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bold: Some(true),
            bg: None,
            italic: None,
            underline: None,
            dim: None,
        },
    );
    let merged = ss.merge_styles(&["a"]);
    assert!(merged.fg.is_some());
    assert!(merged.bg.is_none());
}

#[test]
fn test_merge_styles_multiple() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style(
        "a",
        &StyleDefinitionInput {
            fg: Some(ColorInput::String("red".into())),
            bg: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
        },
    );
    let _ = ss.register_style(
        "b",
        &StyleDefinitionInput {
            bg: Some(ColorInput::String("blue".into())),
            italic: Some(true),
            fg: None,
            bold: None,
            underline: None,
            dim: None,
        },
    );
    let merged = ss.merge_styles(&["a", "b"]);
    assert_eq!(merged.fg.unwrap().to_ints(), (255, 0, 0, 255));
    assert_eq!(merged.bg.unwrap().to_ints(), (0, 0, 255, 255));
}

#[test]
fn test_merge_styles_caches() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("a", &EMPTY_STYLE);
    let _ = ss.merge_styles(&["a"]);
    assert_eq!(ss.get_cache_size(), 1);
    let _ = ss.merge_styles(&["a"]);
    assert_eq!(ss.get_cache_size(), 1);
}

#[test]
fn test_clear_cache() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("a", &EMPTY_STYLE);
    let _ = ss.merge_styles(&["a"]);
    assert_eq!(ss.get_cache_size(), 1);
    ss.clear_cache();
    assert_eq!(ss.get_cache_size(), 0);
}

#[test]
fn test_clear_name_cache() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("test", &EMPTY_STYLE);
    assert!(ss.resolve_style_id("test").is_some());
    ss.clear_name_cache();
    assert!(ss.resolve_style_id("test").is_none());
    assert!(ss.get_style_id("test").is_none());
}

#[test]
fn test_get_all_styles() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("a", &EMPTY_STYLE);
    let _ = ss.register_style("b", &EMPTY_STYLE);
    let all = ss.get_all_styles();
    assert_eq!(all.len(), 2);
    assert!(all.contains_key("a"));
}

#[test]
fn test_get_registered_names() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("x", &EMPTY_STYLE);
    let _ = ss.register_style("y", &EMPTY_STYLE);
    let names = ss.get_registered_names();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"x".to_string()));
}

#[test]
fn test_destroy_clears_internals() {
    let mut ss = SyntaxStyle::create();
    let _ = ss.register_style("a", &EMPTY_STYLE);
    assert_eq!(ss.get_style_count(), 1);
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
    let _ = ss.get_style_count();
}

#[test]
fn test_from_styles() {
    let styles = {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "keyword".into(),
            StyleDefinitionInput {
                fg: Some(ColorInput::String("red".into())),
                bold: Some(true),
                bg: None,
                italic: None,
                underline: None,
                dim: None,
            },
        );
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
            bold: None,
            italic: None,
            underline: None,
            dim: None,
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
            italic: None,
            underline: None,
            dim: None,
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
            background: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
        },
    }];
    let mut ss = SyntaxStyle::from_theme(&theme);
    assert_eq!(ss.get_style_count(), 1);
    let merged = ss.merge_styles(&["keyword"]);
    assert!(merged.fg.is_some());
}
