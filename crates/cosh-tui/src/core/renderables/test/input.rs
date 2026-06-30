use crate::core::renderables::input::InputRenderable;

#[test]
fn test_set_min_length_within_max_length() {
    let mut input = InputRenderable::new(None);
    input.set_min_length(10);
    assert_eq!(input.min_length(), 10);
}

#[test]
#[should_panic(
    expected = "InputRenderable: min_length (1001) cannot be greater than max_length (1000)"
)]
fn test_set_min_length_greater_than_max_length_panics() {
    let mut input = InputRenderable::new(None);
    input.set_min_length(1001);
}
