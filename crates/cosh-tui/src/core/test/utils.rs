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
