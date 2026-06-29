use crate::core::renderable::Renderable;
use crate::core::renderables::r#box::BoxRenderable;

#[test]
fn test_box_new_defaults() {
    let b = BoxRenderable::new();
    assert!(b.id().starts_with("box-"));
    assert!(b.is_visible());
    assert!(!b.is_focusable());
    assert!(!b.is_destroyed());
    assert!(b.parent_num().is_none());
}

#[test]
fn test_box_default_trait() {
    let b = BoxRenderable::default();
    assert!(b.id().starts_with("box-"));
}

#[test]
fn test_box_set_background_color() {
    let mut b = BoxRenderable::new();
    b.set_background_color(Some("#ff0000".into()));
    assert_eq!(b.background_color().r(), 1.0);
    assert_eq!(b.background_color().g(), 0.0);
    assert_eq!(b.background_color().b(), 0.0);
}

#[test]
fn test_box_border_operations() {
    let mut b = BoxRenderable::new();
    b.set_border(true);
    assert!(
        b.border_sides().top
            || b.border_sides().right
            || b.border_sides().bottom
            || b.border_sides().left
    );
    b.set_border(false);
    assert!(
        !b.border_sides().top
            && !b.border_sides().right
            && !b.border_sides().bottom
            && !b.border_sides().left
    );

    b.set_border_style(crate::core::border::BorderStyle::Double);
    assert_eq!(b.border_style(), crate::core::border::BorderStyle::Double);

    b.set_border_color(Some("#00ff00".into()));
    assert_eq!(b.border_color().g(), 1.0);
}

#[test]
fn test_box_title_operations() {
    let mut b = BoxRenderable::new();
    b.set_title(Some("Hello".into()));
    assert_eq!(b.title(), Some("Hello"));

    b.set_title_alignment(crate::core::renderables::r#box::TitleAlignment::Center);
    assert_eq!(
        b.title_alignment(),
        crate::core::renderables::r#box::TitleAlignment::Center
    );

    b.set_bottom_title(Some("Footer".into()));
    assert_eq!(b.bottom_title(), Some("Footer"));
}

#[test]
fn test_box_gap_getters_setters() {
    let mut b = BoxRenderable::new();
    assert!(b.gap().is_none());
    assert!(b.row_gap().is_none());
    assert!(b.column_gap().is_none());

    b.set_gap(Some(4.0));
    assert_eq!(b.gap(), Some(4.0));

    b.set_row_gap(Some(2.0));
    assert_eq!(b.row_gap(), Some(2.0));

    b.set_column_gap(Some(3.0));
    assert_eq!(b.column_gap(), Some(3.0));
}

#[test]
fn test_box_to_taffy_style_default() {
    let b = BoxRenderable::new();
    let style = b.to_taffy_style();
    assert_eq!(style.display, taffy::Display::Flex);
    assert_eq!(style.flex_direction, taffy::FlexDirection::Column);
}

#[test]
fn test_box_to_taffy_style_with_border() {
    let mut b = BoxRenderable::new();
    b.set_border(true);
    let style = b.to_taffy_style();
    assert_eq!(style.border.left, taffy::LengthPercentage::length(1.0));
    assert_eq!(style.border.right, taffy::LengthPercentage::length(1.0));
    assert_eq!(style.border.top, taffy::LengthPercentage::length(1.0));
    assert_eq!(style.border.bottom, taffy::LengthPercentage::length(1.0));
}

#[test]
fn test_box_to_taffy_style_with_gap() {
    let mut b = BoxRenderable::new();
    b.set_gap(Some(8.0));
    let style = b.to_taffy_style();
    assert_eq!(style.gap.width, taffy::LengthPercentage::length(8.0));
    assert_eq!(style.gap.height, taffy::LengthPercentage::length(8.0));
}

#[test]
fn test_box_to_taffy_style_with_row_gap() {
    let mut b = BoxRenderable::new();
    b.set_row_gap(Some(4.0));
    let style = b.to_taffy_style();
    assert_eq!(style.gap.height, taffy::LengthPercentage::length(4.0));
    assert_eq!(style.gap.width, taffy::LengthPercentage::length(0.0));
}

#[test]
fn test_box_to_taffy_style_with_column_gap() {
    let mut b = BoxRenderable::new();
    b.set_column_gap(Some(5.0));
    let style = b.to_taffy_style();
    assert_eq!(style.gap.width, taffy::LengthPercentage::length(5.0));
    assert_eq!(style.gap.height, taffy::LengthPercentage::length(0.0));
}
