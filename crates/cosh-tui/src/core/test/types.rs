use crate::core::types::{
    ATTRIBUTE_BASE_BITS, ATTRIBUTE_BASE_MASK, CURSOR_STYLE_BLOCK, CURSOR_STYLE_DEFAULT,
    DebugOverlayCorner, MOUSE_POINTER_DEFAULT, TERMINAL_CAPABILITY_STATE_SUPPORTED,
    TERMINAL_CAPABILITY_STATE_UNKNOWN, TERMINAL_CAPABILITY_STATE_UNSUPPORTED,
    TERMINAL_MULTIPLEXER_NONE, TERMINAL_MULTIPLEXER_TMUX, THEME_MODE_DARK, THEME_MODE_LIGHT,
    TargetChannel, TextAttributes, WIDTH_METHOD_UNICODE, WIDTH_METHOD_WCWIDTH, get_base_attributes,
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
