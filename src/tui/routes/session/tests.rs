#[test]
fn old_byte_slice_panics_on_multi_byte_char_at_32() {
    let text = "ééééééééééééééé\u{1f60a}"; // 15× é (2B) + 😊 (4B) = 34 bytes
    assert!(text.len() > 32);
    std::panic::catch_unwind(|| {
        _ = &text[..text.len().min(32)];
    })
    .expect_err("slicing at byte 32 inside 😊 should have panicked");
}

#[test]
fn floor_char_boundary_does_not_panic_on_multi_byte_char_at_32() {
    let text = "ééééééééééééééé\u{1f60a}";
    assert!(text.len() > 32);
    let boundary = text.floor_char_boundary(32);
    assert_eq!(&text[..boundary], "ééééééééééééééé");
}

#[test]
fn floor_char_boundary_works_with_short_text() {
    let text = "hello";
    let boundary = text.floor_char_boundary(32);
    assert_eq!(boundary, text.len());
    assert_eq!(&text[..boundary], "hello");
}
