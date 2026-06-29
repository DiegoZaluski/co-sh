use crate::core::renderable::Renderable;
use crate::core::renderables::slider::{SliderOrientation, SliderRenderable};
use crate::core::rgba::RGBA;

#[test]
fn test_new_horizontal() {
    let s = SliderRenderable::new(SliderOrientation::Horizontal);
    assert_eq!(s.value(), 0.0);
    assert_eq!(s.min(), 0.0);
    assert_eq!(s.max(), 100.0);
    assert_eq!(s.orientation(), SliderOrientation::Horizontal);
}

#[test]
fn test_new_vertical() {
    let s = SliderRenderable::new(SliderOrientation::Vertical);
    assert_eq!(s.orientation(), SliderOrientation::Vertical);
}

#[test]
fn test_set_value() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_value(50.0);
    assert!((s.value() - 50.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_value_clamps_to_min() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_value(-10.0);
    assert!((s.value() - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_value_clamps_to_max() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_value(200.0);
    assert!((s.value() - 100.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_min() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_min(10.0);
    assert!((s.min() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_min_clamps_value() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_value(5.0);
    s.set_min(10.0);
    assert!((s.value() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_max() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_max(200.0);
    assert!((s.max() - 200.0).abs() < f64::EPSILON);
}

#[test]
fn test_set_max_clamps_value() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    s.set_value(150.0);
    s.set_max(100.0);
    assert!((s.value() - 100.0).abs() < f64::EPSILON);
}

#[test]
fn test_view_port_size_default() {
    let s = SliderRenderable::new(SliderOrientation::Horizontal);
    assert!((s.view_port_size() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_orientation_switch() {
    let mut s = SliderRenderable::new(SliderOrientation::Horizontal);
    assert!(s.is_focusable());
    assert!(s.is_visible());
    s.set_orientation(SliderOrientation::Vertical);
    assert_eq!(s.orientation(), SliderOrientation::Vertical);
}

#[test]
fn test_track_color_default() {
    let s = SliderRenderable::new(SliderOrientation::Horizontal);
    assert_eq!(s.track_color(), RGBA::from_ints(37, 37, 39, 255));
}

#[test]
fn test_thumb_color_default() {
    let s = SliderRenderable::new(SliderOrientation::Horizontal);
    assert_eq!(s.thumb_color(), RGBA::from_ints(154, 158, 163, 255));
}
