use crate::core::renderable::Renderable;
use crate::core::renderables::scroll_bar::{ScrollBarOrientation, ScrollBarRenderable};

#[test]
fn test_new_vertical() {
    let sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    assert!((sb.scroll_position() - 0.0).abs() < f64::EPSILON);
    assert!((sb.scroll_size() - 0.0).abs() < f64::EPSILON);
    assert!((sb.viewport_size() - 10.0).abs() < f64::EPSILON);
    assert!(sb.is_focusable());
}

#[test]
fn test_new_horizontal() {
    let sb = ScrollBarRenderable::new(ScrollBarOrientation::Horizontal);
    assert_eq!(sb.scroll_position(), 0.0);
}

#[test]
fn test_set_scroll_position() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(100.0);
    sb.set_viewport_size(20.0);
    sb.set_scroll_position(50.0);
    assert!((sb.scroll_position() - 50.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_scroll_position_clamps() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(100.0);
    sb.set_viewport_size(20.0);
    sb.set_scroll_position(200.0);
    assert!((sb.scroll_position() - 80.0).abs() < f64::EPSILON);
    sb.set_scroll_position(-10.0);
    assert!((sb.scroll_position() - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_scroll_size() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(200.0);
    assert!((sb.scroll_size() - 200.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_viewport_size() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_viewport_size(30.0);
    assert!((sb.viewport_size() - 30.0).abs() < f64::EPSILON);
}

#[test]
fn test_scroll_by_absolute() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(100.0);
    sb.set_viewport_size(20.0);
    sb.scroll_by(10.0, false);
    assert!((sb.scroll_position() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_scroll_by_relative() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(100.0);
    sb.set_viewport_size(20.0);
    sb.scroll_by(0.5, true);
    assert!((sb.scroll_position() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_scroll_by_clamps() {
    let mut sb = ScrollBarRenderable::new(ScrollBarOrientation::Vertical);
    sb.set_scroll_size(100.0);
    sb.set_viewport_size(20.0);
    sb.scroll_by(1000.0, false);
    assert!((sb.scroll_position() - 80.0).abs() < f64::EPSILON);
}
